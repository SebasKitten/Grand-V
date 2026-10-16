use gta_browser_launcher::config;
use std::{fs, path::PathBuf};

#[test]
fn branded_folder_and_incomplete_download_errors() {
    let root: PathBuf =
        std::env::temp_dir().join(format!("grandv-assets-test-{}", std::process::id()));
    let assets = root.join("GrandV-assets");
    fs::create_dir_all(assets.join("mirror/playgta5.com")).unwrap();
    fs::write(assets.join("mirror/playgta5.com/launcher.html"), b"ok").unwrap();
    assert_eq!(
        config::resolve_folder(&root).unwrap(),
        assets.canonicalize().unwrap()
    );
    let marker = assets.join("grandv-assets.json");
    fs::write(&marker, r#"{"format":"grandv-assets-v1","files":[["mirror/playgta5.com/launcher.html",2],["missing.rpf",4]]}"#).unwrap();
    assert!(
        config::validate_asset_package(&assets)
            .unwrap_err()
            .to_string()
            .contains("missing.rpf")
    );
    fs::write(assets.join("missing.rpf"), b"bad").unwrap();
    assert!(config::validate_asset_package(&assets).is_err());
    fs::write(assets.join("missing.rpf"), b"good").unwrap();
    assert!(config::validate_asset_package(&assets).is_ok());
    fs::write(
        &marker,
        r#"{"format":"grandv-assets-v1","files":[["../outside",1]]}"#,
    )
    .unwrap();
    assert!(config::validate_asset_package(&assets).is_err());
    fs::remove_file(&marker).unwrap();
    assert!(config::validate_asset_package(&assets).is_ok());
    fs::remove_dir_all(root).unwrap();
}
