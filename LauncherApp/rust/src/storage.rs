use anyhow::{Result, bail};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const MAX_FILE: usize = 32 * 1024 * 1024;
pub const MAX_PROFILE: usize = 64 * 1024 * 1024;
pub fn millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn atomic_write(path: &Path, payload: &[u8], private: bool) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Missing parent"))?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if private {
            file.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }
    #[cfg(not(unix))]
    let _ = private;
    file.write_all(payload)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}
pub fn valid_path(path: &str) -> bool {
    if !path.starts_with("/userdata/") || path.len() > 1024 || path.contains(['\\', '\0']) {
        return false;
    }
    path[10..].split('/').all(|p| {
        let name = p.split('.').next().unwrap_or("").to_ascii_uppercase();
        !p.is_empty()
            && p != "."
            && p != ".."
            && !p.contains(':')
            && !p.ends_with([' ', '.'])
            && !p.chars().any(char::is_control)
            && !["CON", "PRN", "AUX", "NUL"].contains(&name.as_str())
            && !(name.len() == 4
                && (name.starts_with("COM") || name.starts_with("LPT"))
                && matches!(name.as_bytes()[3], b'1'..=b'9'))
            && p != ".tombstones.json"
    })
}
pub fn profile_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
// Check every existing ancestor: a symlink/junction must never make a save escape its profile.
pub fn safe_target(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative.contains(['\\', '\0', ':'])
        || relative
            .split('/')
            .any(|p| p == ".." || p == "." || p.is_empty())
    {
        bail!("Invalid path");
    }
    let canonical = root.canonicalize()?;
    let target = canonical.join(relative);
    for parent in target.ancestors() {
        if parent.exists() && !parent.canonicalize()?.starts_with(&canonical) {
            bail!("Path escapes root");
        }
        if parent == canonical {
            break;
        }
    }
    Ok(target)
}
pub fn userdata_target(folder: &Path, path: &str) -> Result<(PathBuf, PathBuf)> {
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 4 || parts[1] != "userdata" || !profile_id(parts[2]) {
        bail!("Invalid userdata URL");
    }
    let decoded = String::from_utf8(URL_SAFE_NO_PAD.decode(parts[3].trim_end_matches('='))?)?;
    if !valid_path(&decoded) {
        bail!("Invalid userdata path");
    }
    let userdata = folder.join("userdata");
    std::fs::create_dir_all(&userdata)?;
    let root = safe_target(&userdata, parts[2])?;
    std::fs::create_dir_all(&root)?;
    let target = safe_target(&root, &decoded[10..])?;
    Ok((root, target))
}
fn tombstones(root: &Path) -> Result<BTreeMap<String, u64>> {
    let path = root.join(".tombstones.json");
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    // Malformed existing tombstones fail closed rather than resurrecting deleted files.
    let v: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    if let Some(a) = v.as_array() {
        return Ok(a
            .iter()
            .filter_map(|v| v.as_str().map(|s| (s.to_string(), 0)))
            .collect());
    }
    let rows = v
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Invalid tombstones"))?;
    rows.iter()
        .map(|(k, v)| {
            Ok((
                k.clone(),
                v.as_u64()
                    .or_else(|| {
                        v.as_f64()
                            .filter(|n| n.is_finite() && *n >= 0.)
                            .map(|n| n as u64)
                    })
                    .ok_or_else(|| anyhow::anyhow!("Invalid tombstone time"))?,
            ))
        })
        .collect()
}
fn update(root: &Path, target: &Path, deleted: bool) -> Result<()> {
    let mut rows = tombstones(root)?;
    let key = target
        .strip_prefix(root)?
        .to_string_lossy()
        .replace('\\', "/");
    if deleted {
        rows.insert(key, millis());
    } else {
        rows.remove(&key);
    }
    atomic_write(
        &root.join(".tombstones.json"),
        &serde_json::to_vec(&rows)?,
        false,
    )
}
pub fn put(folder: &Path, path: &str, body: &[u8], mtime: Option<f64>) -> Result<()> {
    if body.len() > MAX_FILE {
        bail!("Save exceeds 32 MiB");
    }
    let (root, target) = userdata_target(folder, path)?;
    let mut size = body.len() as u64;
    for entry in walkdir::WalkDir::new(&root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file()
            && entry.path() != target
            && entry.file_name() != ".tombstones.json"
        {
            size += entry.metadata()?.len();
        }
    }
    if size > MAX_PROFILE as u64 {
        bail!("Profile exceeds 64 MiB");
    }
    // Validate the existing tombstone file before committing a new save.
    tombstones(&root)?;
    atomic_write(&target, body, false)?;
    if let Some(ms) = mtime.filter(|v| v.is_finite() && *v > 0. && *v < 4102444800000.) {
        filetime::set_file_mtime(
            &target,
            filetime::FileTime::from_unix_time(
                (ms / 1000.) as i64,
                ((ms % 1000.) * 1_000_000.) as u32,
            ),
        )?;
    }
    update(&root, &target, false)
}
pub fn delete(folder: &Path, path: &str) -> Result<()> {
    let (root, target) = userdata_target(folder, path)?;
    update(&root, &target, true)?;
    match std::fs::remove_file(target) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
pub fn restore(folder: &Path, id: &str) -> Result<Value> {
    if !profile_id(id) {
        bail!("Invalid profile");
    }
    let userdata = folder.join("userdata");
    if !userdata.exists() {
        return Ok(json!({"files":[],"tombstones":[]}));
    }
    let root = safe_target(&userdata, id)?;
    let deleted = tombstones(&root)?;
    let mut files = Vec::new();
    let mut total = 0;
    if root.exists() {
        for entry in walkdir::WalkDir::new(&root).follow_links(false) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry
                .path()
                .strip_prefix(&root)?
                .to_string_lossy()
                .replace('\\', "/");
            let game_path = format!("/userdata/{rel}");
            if deleted.contains_key(&rel) || !valid_path(&game_path) {
                continue;
            }
            let size = entry.metadata()?.len() as usize;
            total += size;
            if size > MAX_FILE || total > MAX_PROFILE {
                bail!("Profile exceeds restore limit");
            }
            let data = std::fs::read(entry.path())?;
            let mtime = entry
                .metadata()?
                .modified()?
                .duration_since(UNIX_EPOCH)?
                .as_millis() as u64;
            files.push(json!({"path":game_path,"data":STANDARD.encode(data),"mtime":mtime}));
        }
    }
    let tombstones: Vec<_> = deleted
        .into_iter()
        .filter(|(p, _)| valid_path(&format!("/userdata/{p}")))
        .map(|(p, t)| json!({"path":format!("/userdata/{p}"),"mtime":t}))
        .collect();
    Ok(json!({"files":files,"tombstones":tombstones}))
}
