use super::*;

/// A staging directory that removes itself on drop (`tempfile.TemporaryDirectory`).
pub(super) struct Scratch {
    path: PathBuf,
}

impl Scratch {
    pub(super) fn new(parent: &Path, prefix: &str) -> ProjectsResult<Self> {
        for _ in 0..100 {
            let candidate = parent.join(format!("{prefix}{}", uuid::Uuid::new_v4().simple()));
            let created = {
                #[cfg(unix)]
                {
                    use spiderhead_core::filesystem::PrivateMode;
                    fs::DirBuilder::new().private_mode(0o700).create(&candidate)
                }
                #[cfg(not(unix))]
                fs::create_dir(&candidate)
            };
            match created {
                Ok(()) => return Ok(Self { path: candidate }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(value("could not create a staging directory"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// `archive_project`: write one zip for a validated project; `dry_run` only
/// reports the plan and size estimate.
pub fn archive_project(
    directory: &Path,
    out: &Path,
    dry_run: bool,
    exclude_bodies: bool,
    exclude_bodies_older_than_days: Option<i64>,
    keep_newest_scans: Option<i64>,
) -> ProjectsResult<Value> {
    let (root, document) = workspace_load(directory)?;
    if !matches!(exclude_bodies_older_than_days, None | Some(30) | Some(90)) {
        return Err(value(
            "exclude_bodies_older_than_days must be 30, 90, or null",
        ));
    }
    if exclude_bodies && exclude_bodies_older_than_days.is_some() {
        return Err(value(
            "choose exclude_bodies or exclude_bodies_older_than_days",
        ));
    }
    if keep_newest_scans.is_some_and(|k| !(1..=10_000).contains(&k)) {
        return Err(value("keep_newest_scans must be from 1 to 10000"));
    }
    validate_project_stores(&root)?;
    let target = out.to_path_buf();
    if target.file_name().is_none() || target.components().any(|c| c.as_os_str() == "..") {
        return Err(value("archive output must be a plain file path"));
    }
    let parent = safe_directory(
        target.parent().unwrap_or_else(|| Path::new(".")),
        "archive output parent",
    )?;
    let (included, skipped) = plan(&root, keep_newest_scans)?;
    let estimate: u64 = included
        .iter()
        .map(|(_, path)| fs::metadata(path).map(|m| m.len()).unwrap_or(0))
        .sum();
    let mut result = json!({
        "ok": true,
        "dry_run": dry_run,
        "files": included.iter().map(|(rel, _)| rel.clone()).collect::<Vec<_>>(),
        "skipped": skipped,
        "estimated_bytes": estimate,
        "exclude_bodies": exclude_bodies,
        "exclude_bodies_older_than_days": exclude_bodies_older_than_days,
        "keep_newest_scans": keep_newest_scans,
    });
    if dry_run {
        return Ok(result);
    }
    if fs::symlink_metadata(&target).is_ok() {
        return Err(value("archive output already exists; choose a new file"));
    }
    let partial = parent.join(format!(
        ".{}.partial",
        target.file_name().unwrap_or_default().to_string_lossy()
    ));
    let mut excluded_body_blobs = 0i64;
    let mut entries: Vec<Value> = Vec::new();
    {
        let scratch = Scratch::new(&parent, ".seohead-archive-")?;
        let mut staged: Vec<(String, PathBuf)> = Vec::new();
        for (rel, path) in &included {
            let copy = scratch.path.join(rel.replace('/', "__"));
            if SQLITE.iter().any(|s| rel.ends_with(s)) {
                snapshot(path, &copy)?;
                if exclude_bodies || exclude_bodies_older_than_days.is_some() {
                    excluded_body_blobs +=
                        exclude_html_bodies(&copy, exclude_bodies_older_than_days)?;
                }
            } else {
                fs::copy(path, &copy)?;
            }
            staged.push((rel.clone(), copy));
        }
        for (rel, copy) in &staged {
            let (digest, size) = sha256_file(copy)?;
            entries.push(json!({"path": rel, "size": size, "sha256": digest}));
        }
        let manifest = json!({
            "format": ARCHIVE_FORMAT,
            "archive_version": ARCHIVE_VERSION,
            "project_format": crate::workspace::PROJECT_FORMAT,
            "seohead_version": spiderhead_core::user_agents::PRODUCT_VERSION,
            "project_uuid": document["project_uuid"],
            "created_at": crate::pytime::now_iso_micros(),
            "directories": DIRECTORIES,
            "files": entries,
        });
        {
            let file = File::create(&partial)?;
            let mut writer = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer
                .start_file(MANIFEST, options)
                .map_err(|e| value(e.to_string()))?;
            writer
                .write_all(crate::pyjson::dumps_indent2(&manifest).as_bytes())
                .map_err(|e| value(e.to_string()))?;
            for (rel, copy) in &staged {
                writer
                    .start_file(rel.as_str(), options)
                    .map_err(|e| value(e.to_string()))?;
                let mut stream = File::open(copy)?;
                std::io::copy(&mut stream, &mut writer).map_err(|e| value(e.to_string()))?;
            }
            writer.finish().map_err(|e| value(e.to_string()))?;
        }
    }
    let link_result = fs::hard_link(&partial, &target);
    let _ = fs::remove_file(&partial);
    link_result?;
    result["dry_run"] = json!(false);
    result["archive"] = json!(target.to_string_lossy());
    result["file_count"] = json!(entries.len());
    result["excluded_body_blobs"] = json!(excluded_body_blobs);
    Ok(result)
}

/// `_member_name`: reject unsafe member paths.
pub(super) fn member_name(name: &str) -> ProjectsResult<String> {
    let parts: Vec<&str> = name.split('/').collect();
    if name.is_empty()
        || name.starts_with('/')
        || name.contains('\\')
        || parts[0].contains(':')
        || parts
            .iter()
            .any(|p| p.is_empty() || *p == "." || *p == "..")
    {
        return Err(value(format!("archive member is unsafe: {name:?}")));
    }
    Ok(name.to_owned())
}

/// `_read_verified_manifest`: `(manifest, listed files)`.
pub(super) fn read_verified_manifest(
    archive: &mut zip::ZipArchive<File>,
) -> ProjectsResult<(Value, Map<String, Value>)> {
    let mut names = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|e| value(e.to_string()))?;
        let name = entry.name().map_err(|e| value(e.to_string()))?.to_string();
        names.push(member_name(&name)?);
        let mode = entry.unix_mode().unwrap_or(0);
        if mode & 0o170000 == 0o120000 {
            return Err(value(format!("archive member is a symlink: {name:?}")));
        }
        if name != MANIFEST && credential_name(&name) {
            return Err(value("archive contains a credential-like file name"));
        }
    }
    if names
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != names.len()
    {
        return Err(value("archive has duplicate members"));
    }
    let manifest: Value = {
        let mut reader = archive
            .by_name(MANIFEST)
            .map_err(|_| value("archive manifest is unsupported"))?;
        let mut text = String::new();
        reader.read_to_string(&mut text)?;
        serde_json::from_str(&text).map_err(|_| value("archive manifest is unsupported"))?
    };
    let listed_ok = manifest.is_object()
        && manifest.get("format").and_then(Value::as_str) == Some(ARCHIVE_FORMAT)
        && manifest.get("archive_version").and_then(Value::as_i64) == Some(ARCHIVE_VERSION)
        && manifest.get("project_format").and_then(Value::as_str)
            == Some(crate::workspace::PROJECT_FORMAT)
        && manifest
            .get("directories")
            .and_then(Value::as_array)
            .map(|d| d.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>() == DIRECTORIES)
            == Some(true)
        && manifest.get("files").is_some_and(|f| f.is_array());
    if !listed_ok {
        return Err(value("archive manifest is unsupported"));
    }
    let mut listed: Map<String, Value> = Map::new();
    for entry in manifest["files"].as_array().unwrap_or(&Vec::new()) {
        if let Some(path) = entry["path"].as_str() {
            listed.insert(path.to_owned(), entry.clone());
        }
    }
    let members: std::collections::BTreeSet<String> = names
        .iter()
        .filter(|n| n.as_str() != MANIFEST)
        .cloned()
        .collect();
    if members != listed.keys().cloned().collect()
        || listed.len() != manifest["files"].as_array().map_or(0, Vec::len)
    {
        return Err(value("archive members do not match the manifest"));
    }
    Ok((manifest, listed))
}

pub(super) fn sha256_member(reader: &mut impl Read) -> ProjectsResult<(String, u64)> {
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        size += read as u64;
    }
    Ok((hex64(&digest.finalize()), size))
}

/// `archive_list`: describe and verify every member against its manifest.
pub fn archive_list(archive: &Path) -> ProjectsResult<Value> {
    if archive.is_symlink() || !archive.is_file() {
        return Err(value("archive must be an existing regular file"));
    }
    let file = File::open(archive)?;
    let mut zip_archive = zip::ZipArchive::new(file).map_err(|e| value(e.to_string()))?;
    let (manifest, listed) = read_verified_manifest(&mut zip_archive)?;
    let mut total_bytes = 0u64;
    for index in 0..zip_archive.len() {
        let mut entry = zip_archive
            .by_index(index)
            .map_err(|e| value(e.to_string()))?;
        let name = entry.name().map_err(|e| value(e.to_string()))?.to_string();
        if name == MANIFEST {
            continue;
        }
        let (digest, size) = sha256_member(&mut entry)?;
        let expected = listed
            .get(name.as_str())
            .ok_or_else(|| value("archive members do not match the manifest"))?;
        if size != expected["size"].as_u64().unwrap_or(u64::MAX)
            || digest != expected["sha256"].as_str().unwrap_or_default()
        {
            return Err(value(format!(
                "archive member failed verification: {name:?}"
            )));
        }
        total_bytes += size;
    }
    Ok(json!({
        "ok": true,
        "archive": archive.to_string_lossy(),
        "format": manifest["format"],
        "archive_version": manifest["archive_version"],
        "seohead_version": manifest["seohead_version"],
        "project_uuid": manifest["project_uuid"],
        "created_at": manifest["created_at"],
        "file_count": manifest["files"].as_array().map_or(0, Vec::len),
        "total_bytes": total_bytes,
        "verified": true,
        "files": manifest["files"],
    }))
}

/// `restore_project`: verify every member against the manifest, then publish the
/// project at a new path.
pub fn restore_project(archive: &Path, to: &Path) -> ProjectsResult<Value> {
    if archive.is_symlink() || !archive.is_file() {
        return Err(value("archive must be an existing regular file"));
    }
    let dest = to.to_path_buf();
    if dest.file_name().is_none()
        || dest.components().any(|c| c.as_os_str() == "..")
        || fs::symlink_metadata(&dest).is_ok()
    {
        return Err(value("restore target must be a new path"));
    }
    let parent = safe_directory(
        dest.parent().unwrap_or_else(|| Path::new(".")),
        "restore parent",
    )?;
    let file = File::open(archive)?;
    let mut zip_archive = zip::ZipArchive::new(file).map_err(|e| value(e.to_string()))?;
    let names: Vec<String> = (0..zip_archive.len())
        .filter_map(|i| {
            zip_archive
                .by_index(i)
                .ok()
                .and_then(|e| e.name().ok().map(|c| c.to_string()))
        })
        .collect();
    let (_manifest, listed) = read_verified_manifest(&mut zip_archive)?;
    {
        let staging = Scratch::new(&parent, ".seohead-restore-")?;
        for name in DIRECTORIES {
            let dir = staging.path.join(name);
            fs::create_dir_all(&dir)?;
            #[cfg(unix)]
            {
                spiderhead_core::filesystem::set_permissions_mode(&dir, 0o700)?;
            }
        }
        for name in &names {
            if name == MANIFEST {
                continue;
            }
            let expected = listed
                .get(name.as_str())
                .ok_or_else(|| value("archive members do not match the manifest"))?;
            let target = staging.path.join(name);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut entry = zip_archive
                .by_name(name)
                .map_err(|e| value(e.to_string()))?;
            let mut digest = Sha256::new();
            let mut size = 0u64;
            let mut buffer = vec![0u8; 1 << 20];
            let mut out = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            loop {
                let read = entry.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                digest.update(&buffer[..read]);
                size += read as u64;
                out.write_all(&buffer[..read])?;
            }
            if size != expected["size"].as_u64().unwrap_or(u64::MAX)
                || hex64(&digest.finalize()) != expected["sha256"].as_str().unwrap_or_default()
            {
                return Err(value(format!(
                    "archive member failed verification: {name:?}"
                )));
            }
        }
        workspace_load(&staging.path)?;
        for name in PROJECT_STATE {
            let path = staging.path.join(name);
            if path.is_file() {
                validate_project_state(&staging.path, name)?;
            }
        }
        validate_project_stores(&staging.path)?;
        if fs::symlink_metadata(&dest).is_ok() {
            return Err(value(
                "restore target appeared during restore; nothing was replaced",
            ));
        }
        fs::rename(&staging.path, &dest)?;
        std::mem::forget(staging);
    }
    Ok(json!({
        "ok": true,
        "restored": dest.to_string_lossy(),
        "project": crate::workspace::open_project(&dest, None)?["project"],
    }))
}

/// `datetime.fromisoformat` + `utcoffset() == UTC` check: returns `true` only for
/// offset-aware stamps whose offset is exactly UTC.
pub(super) fn parse_utc_stamp(text: &str) -> Result<bool, ()> {
    let (offset_secs, body_len) = if text.ends_with('Z') {
        (0i64, text.len() - 1)
    } else {
        let bytes = text.as_bytes();
        let mut marker = None;
        for (index, byte) in bytes.iter().enumerate().skip(10) {
            if *byte == b'+' || *byte == b'-' {
                marker = Some(index);
            }
        }
        let Some(index) = marker else {
            // Naive stamps have `tzinfo=None` and fail the UTC check.
            return text
                .parse::<jiff::civil::DateTime>()
                .map(|_| false)
                .map_err(|_| ());
        };
        let off_text = &text[index..];
        let (digits, frac) = off_text[1..]
            .split_once('.')
            .unwrap_or((&off_text[1..], ""));
        let parts: Vec<&str> = digits.split(':').collect();
        let total = match parts.as_slice() {
            [hh, mm] => hh
                .parse::<i64>()
                .ok()
                .zip(mm.parse::<i64>().ok())
                .map(|(h, m)| h * 3600 + m * 60),
            [hh, mm, ss] => hh
                .parse::<i64>()
                .ok()
                .zip(mm.parse::<i64>().ok())
                .zip(ss.parse::<i64>().ok())
                .map(|((h, m), s)| h * 3600 + m * 60 + s),
            [basic] if basic.len() == 4 => {
                let (hh, mm) = basic.split_at(2);
                hh.parse::<i64>()
                    .ok()
                    .zip(mm.parse::<i64>().ok())
                    .map(|(h, m)| h * 3600 + m * 60)
            }
            [basic] if basic.len() == 6 => {
                let (hh, rest) = basic.split_at(2);
                let (mm, ss) = rest.split_at(2);
                hh.parse::<i64>()
                    .ok()
                    .zip(mm.parse::<i64>().ok())
                    .zip(ss.parse::<i64>().ok())
                    .map(|((h, m), s)| h * 3600 + m * 60 + s)
            }
            [basic] if basic.len() == 2 => basic.parse::<i64>().ok().map(|h| h * 3600),
            _ => None,
        };
        let Some(magnitude) = total else {
            return Err(());
        };
        if frac.parse::<f64>().map(|f| f != 0.0).unwrap_or(false) && !frac.is_empty() {
            return Err(());
        }
        if off_text.len() < 3 {
            return Err(());
        }
        (
            (if bytes[index] == b'-' {
                -magnitude
            } else {
                magnitude
            }),
            index,
        )
    };
    let _ = body_len;
    let body = &text[..body_len];
    body.parse::<jiff::civil::DateTime>()
        .map(|_| offset_secs == 0)
        .map_err(|_| ())
}
