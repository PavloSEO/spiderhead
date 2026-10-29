//! Oracle parity for `spiderconfig` (sf-core-pack-05): the Java-serialization
//! boolean/speed reader, the nine-byte speed patch, config discovery and the
//! throttled-config builder. Regenerate with the reference suite.
//!
//! Every `expect.ok` / `expect.error` is the oracle's own output — errors are
//! compared on the exact message text (`TypeName: message`).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use spiderhead_sf_import::spiderconfig::{
    build_throttled_config_in, find_base_config_in, patch_speed, read_boolean_field,
    read_module_flags, read_speed,
};

fn cases() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/spiderconfig.json");
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn fs_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/spiderconfig_fs")
}

fn b64d(s: &str) -> Vec<u8> {
    const T: &[i8; 128] = &{
        let mut t = [-1i8; 128];
        let mut i = 0usize;
        while i < 64 {
            t[b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"[i] as usize] =
                i as i8;
            i += 1;
        }
        t
    };
    let bytes: Vec<u8> = s.bytes().filter(|&b| b != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut acc = 0u32;
        for (i, &b) in chunk.iter().enumerate() {
            acc |= (T[b as usize] as u32) << (18 - 6 * i);
        }
        out.push((acc >> 16) as u8);
        if chunk.len() > 2 {
            out.push((acc >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(acc as u8);
        }
    }
    out
}

fn b64e(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for chunk in b.chunks(3) {
        let (a, b, c) = (
            chunk[0] as u32,
            *chunk.get(1).unwrap_or(&0) as u32,
            *chunk.get(2).unwrap_or(&0) as u32,
        );
        s.push(A[(a >> 2) as usize] as char);
        s.push(A[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        s.push(if chunk.len() > 1 {
            A[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        s.push(if chunk.len() > 2 {
            A[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    s
}

fn parse_rate(v: &Value) -> f64 {
    // reference float repr: "nan", "inf", "-inf", or a plain number.
    let s = v
        .as_str()
        .unwrap_or_else(|| panic!("rate is not a repr string: {v}"));
    s.parse::<f64>()
        .unwrap_or_else(|_| panic!("cannot parse rate repr {s:?}"))
}

fn sub(p: &str, root: &Path) -> String {
    p.replace("{root}", &root.display().to_string())
}

fn check(expect: &Value, got: Result<Value, String>, name: &str) {
    match (expect.get("ok"), expect.get("error")) {
        (Some(want), _) => {
            let got = got.unwrap_or_else(|e| panic!("{name}: want ok {want}, got error {e}"));
            assert_eq!(&got, want, "{name}");
        }
        (_, Some(want)) => match got {
            Err(e) => {
                let want = want.as_str().unwrap();
                let want = want.split_once(": ").map(|(_, m)| m).unwrap_or(want);
                assert_eq!(&e, want, "{name}")
            }
            Ok(v) => panic!("{name}: want error {want}, got ok {v}"),
        },
        _ => panic!("{name}: malformed expect {expect}"),
    }
}

#[test]
fn oracle_cases() {
    for case in cases().as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let kind = case["kind"].as_str().unwrap();
        match kind {
            // test, incl. descriptor edge cases the tests build ad hoc.
            "read_boolean_field" => {
                let got = read_boolean_field(
                    &b64d(case["blob"].as_str().unwrap()),
                    case["class"].as_str().unwrap(),
                    case["field"].as_str().unwrap(),
                )
                .map(Value::Bool)
                .map_err(|e| e.to_string());
                check(&case["expect"], got, name);
            }
            // test_read_module_flags_never_raises_on_garbage}
            "read_module_flags" => {
                let flags = read_module_flags(&b64d(case["blob"].as_str().unwrap()));
                let got = flags
                    .iter()
                    .map(|(k, v)| (k.to_string(), json!(v)))
                    .collect::<serde_json::Map<String, Value>>();
                let want = case["expect"]["ok"].as_object().unwrap().clone();
                assert_eq!(got, want, "{name}");
            }
            "read_speed" => {
                let got = read_speed(&b64d(case["blob"].as_str().unwrap()))
                    .map(|(on, rate)| json!([on, rate]))
                    .map_err(|e| e.to_string());
                check(&case["expect"], got, name);
            }
            "patch_speed" => {
                let got = patch_speed(
                    &b64d(case["blob"].as_str().unwrap()),
                    parse_rate(&case["rate"]),
                )
                .map(|b| json!(b64e(&b)))
                .map_err(|e| e.to_string());
                check(&case["expect"], got, name);
            }
            "find_base_config" => {
                let root = fs_root();
                let patterns: Vec<String> = case["patterns"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| sub(p.as_str().unwrap(), &root))
                    .collect();
                let explicit = case["explicit"].as_str().map(|p| sub(p, &root));
                let got = find_base_config_in(explicit.as_deref(), &patterns).map(|p| {
                    p.display()
                        .to_string()
                        .replace(&root.display().to_string(), "{root}")
                });
                let want = &case["expect"]["ok"];
                assert_eq!(
                    got.map(Value::String).unwrap_or(Value::Null),
                    *want,
                    "{name}"
                );
            }
            "build_throttled_config" => {
                let root = fs_root();
                let tmp = std::env::temp_dir().join(format!("spc-{name}"));
                let dest = tmp.join(
                    Path::new(case["dest"].as_str().unwrap())
                        .file_name()
                        .unwrap(),
                );
                let base = sub(case["base"].as_str().unwrap(), &root);
                let got = build_throttled_config_in(
                    &dest,
                    parse_rate(&case["rate"]),
                    Some(&base),
                    &[sub("{root}/none/*.cfg", &root)],
                )
                .map_err(|e| e.to_string())
                .map(|abs| {
                    let written = fs::read(&dest).unwrap_or_default();
                    json!({
                        "ok": abs.display().to_string(),
                        "written": b64e(&written),
                    })
                });
                match got {
                    Err(e) => check(&case["expect"], Err(e), name),
                    Ok(v) => {
                        // The oracle returns os.path.abspath(dest): for an
                        // absolute dest that is dest itself.
                        assert_eq!(
                            v["ok"].as_str().unwrap(),
                            dest.display().to_string(),
                            "{name} abspath"
                        );
                        assert_eq!(
                            v["written"].as_str().unwrap(),
                            case["expect"]["written"].as_str().unwrap(),
                            "{name} written bytes"
                        );
                    }
                }
            }
            other => panic!("unknown case kind {other}"),
        }
    }
}
