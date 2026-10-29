//! `cargo xtask oracle --slice <id> [--site <name>] [--baseline-dir D] [--actual-dir D]
//!  [--known F] [--float-tol X]` and `cargo xtask oracle --self-test`.
//!
//! Convention: `oracle/actual/<slice>/**/*.json` mirrors `oracle/baselines/<site>/**`.
//! Missing baseline dir: SKIP (exit 0, reason printed). Missing actual file: FAIL.
use serde_json::Value;
use spiderhead_oracle_diff::{diff, report, KnownDivergences, Options};
use std::{
    fs,
    path::{Path, PathBuf},
};

const SHOWN: usize = 20;

#[derive(Default)]
struct Args {
    slice: Option<String>,
    site: Option<String>,
    baseline_dir: Option<String>,
    actual_dir: Option<String>,
    known: Option<String>,
    float_tol: f64,
    self_test: bool,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = args.iter();
    while let Some(f) = it.next() {
        let mut val = |n: &str| it.next().cloned().ok_or(format!("{n} needs a value"));
        match f.as_str() {
            "--slice" => a.slice = Some(val("--slice")?),
            "--site" => a.site = Some(val("--site")?),
            "--baseline-dir" => a.baseline_dir = Some(val("--baseline-dir")?),
            "--actual-dir" => a.actual_dir = Some(val("--actual-dir")?),
            "--known" => a.known = Some(val("--known")?),
            "--float-tol" => {
                a.float_tol = val("--float-tol")?.parse().map_err(|e| format!("{e}"))?
            }
            "--self-test" => a.self_test = true,
            o => return Err(format!("unknown option {o}")),
        }
    }
    if !a.self_test && a.slice.is_none() {
        return Err("need --slice <id> or --self-test".into());
    }
    Ok(a)
}

pub fn run(args: &[String]) -> bool {
    let a = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("oracle: {e}\nusage: cargo xtask oracle (--slice <id> [--site S] | --self-test) [--baseline-dir D] [--actual-dir D] [--known F] [--float-tol X]");
            return false;
        }
    };
    let base = PathBuf::from(a.baseline_dir.as_deref().unwrap_or("oracle/baselines"));
    let known_path = a
        .known
        .as_deref()
        .unwrap_or("oracle/known-divergences.toml");
    let known = match load_known(Path::new(known_path)) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("oracle: {known_path}: {e}");
            return false;
        }
    };
    let opts = Options {
        float_tolerance: a.float_tol,
        ..Default::default()
    };
    let mut used = vec![];
    let ok = if a.self_test {
        self_test(&base, &opts)
    } else {
        let slice = a.slice.as_deref().unwrap();
        let site = a.site.clone().unwrap_or_else(|| site_for(slice));
        let actual = PathBuf::from(a.actual_dir.as_deref().unwrap_or("oracle/actual")).join(slice);
        compare_slice(slice, &base.join(&site), &actual, &opts, &known, &mut used)
    };
    if !a.self_test {
        for e in known.stale(&used) {
            println!(
                "WARN stale known-divergence: id={} file={} path={}",
                e.id, e.file, e.path
            );
        }
    }
    ok
}

fn load_known(p: &Path) -> Result<KnownDivergences, String> {
    match fs::read_to_string(p) {
        Ok(t) => KnownDivergences::from_toml(&t),
        Err(_) => Ok(KnownDivergences::default()),
    }
}

/// Site of a slice: `oracle_site` in slices.toml if present, else the slice id.
fn site_for(slice: &str) -> String {
    fs::read_to_string("slices.toml")
        .ok()
        .and_then(|t| t.parse::<toml::Value>().ok())
        .and_then(|d| {
            d.get("slice")?.as_array()?.iter().find_map(|s| {
                (s.get("id")?.as_str()? == slice)
                    .then(|| s.get("oracle_site")?.as_str().map(String::from))
                    .flatten()
            })
        })
        .unwrap_or_else(|| slice.to_string())
}

fn json_files(root: &Path) -> Vec<PathBuf> {
    fn rec(d: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                rec(&p, out);
            } else if p.extension().is_some_and(|x| x == "json") {
                out.push(p);
            }
        }
    }
    let mut v = vec![];
    rec(root, &mut v);
    v.sort();
    v
}

fn read_json(p: &Path) -> Result<Value, String> {
    let t = fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
    serde_json::from_str(&t).map_err(|e| format!("{}: {e}", p.display()))
}

fn compare_slice(
    slice: &str,
    base: &Path,
    actual: &Path,
    opts: &Options,
    known: &KnownDivergences,
    used: &mut Vec<bool>,
) -> bool {
    if !base.is_dir() {
        println!(
            "SKIP {slice}: baseline dir {} not found (no oracle baseline recorded)",
            base.display()
        );
        return true;
    }
    let site = base
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let files = json_files(base);
    if files.is_empty() {
        eprintln!(
            "FAIL {slice}: baseline dir {} has no *.json",
            base.display()
        );
        return false;
    }
    let (mut bad, mut ok_n) = (0, 0);
    for f in &files {
        let rel = f
            .strip_prefix(base)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let check = rel.as_str();
        let result = (|| {
            let exp = read_json(f)?;
            let ap = actual.join(&rel);
            if !ap.is_file() {
                return Err(format!("missing actual output {}", ap.display()));
            }
            let act = read_json(&ap)?;
            Ok(known.filter(&site, check, diff(&exp, &act, opts), used))
        })();
        match result {
            Ok(d) if d.is_empty() => ok_n += 1,
            Ok(d) => {
                bad += 1;
                println!("DIFF {slice}/{rel}\n{}", report(&d, SHOWN));
            }
            Err(e) => {
                bad += 1;
                println!("FAIL {slice}/{rel}: {e}");
            }
        }
    }
    println!(
        "oracle {slice}: {ok_n} ok, {bad} failing of {} file(s)",
        files.len()
    );
    bad == 0
}

fn self_test(base: &Path, opts: &Options) -> bool {
    let files = json_files(base);
    if files.is_empty() {
        println!("SKIP self-test: no baselines under {}", base.display());
        return true;
    }
    let mut bad = 0;
    for f in &files {
        match read_json(f) {
            Ok(v) => {
                let d = diff(&v, &v, opts);
                if !d.is_empty() {
                    bad += 1;
                    println!("DIFF {}\n{}", f.display(), report(&d, SHOWN));
                }
            }
            Err(e) => {
                bad += 1;
                println!("FAIL {e}");
            }
        }
    }
    println!("self-test: {} file(s), {bad} failing", files.len());
    bad == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xtask-oracle-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }

    #[test]
    fn skip_fail_pass() {
        let d = tmp("cmp");
        let (b, a) = (d.join("b/site"), d.join("a/s"));
        let (o, k) = (Options::default(), KnownDivergences::default());
        let mut u = vec![];
        // missing baseline dir: skip
        assert!(compare_slice("s", &b, &a, &o, &k, &mut u));
        // empty baseline dir: fail
        fs::create_dir_all(&b).unwrap();
        assert!(!compare_slice("s", &b, &a, &o, &k, &mut u));
        // missing actual: fail
        put(&b.join("x/p.json"), r#"{"a":1}"#);
        assert!(!compare_slice("s", &b, &a, &o, &k, &mut u));
        // diff: fail
        put(&a.join("x/p.json"), r#"{"a":2}"#);
        assert!(!compare_slice("s", &b, &a, &o, &k, &mut u));
        // equal: pass
        put(&a.join("x/p.json"), r#"{"a":1}"#);
        assert!(compare_slice("s", &b, &a, &o, &k, &mut u));
        // known divergence approves a diff
        put(&a.join("x/p.json"), r#"{"a":2}"#);
        let k = KnownDivergences::from_toml(
            "[[divergence]]\nid=\"D-1\"\nfile=\"x/p.json\"\napproved_on=\"2026-10-27\"\npath=\"/a\"\nreason=\"r\"\napproved_by=\"p\"\n",
        )
        .unwrap();
        assert!(compare_slice("s", &b, &a, &o, &k, &mut u));
        assert!(k.stale(&u).is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn self_test_ok() {
        let d = tmp("self");
        assert!(self_test(&d, &Options::default())); // skip
        put(&d.join("s/a.json"), r#"{"a":[1,2.5,{"b":null}]}"#);
        assert!(self_test(&d, &Options::default()));
        put(&d.join("s/bad.json"), "{");
        assert!(!self_test(&d, &Options::default()));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn arg_parsing() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(parse(&s(&[])).is_err());
        assert!(parse(&s(&["--bogus"])).is_err());
        let a = parse(&s(&["--slice", "r2", "--float-tol", "1e-6"])).unwrap();
        assert_eq!((a.slice.as_deref(), a.float_tol), (Some("r2"), 1e-6));
        assert!(parse(&s(&["--self-test"])).unwrap().self_test);
    }
}
