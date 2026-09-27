//! Ensure a provisioned CLI still discovers its outer package and install method.

use super::*;
use pretty_assertions::assert_eq;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

const COMPUTER_USE_MCP_RESOURCE_NAME: &str = "codex-computer-use-mcp";
const SKY_RESOURCE_NAME: &str = "sky_linux_x64";

#[test]
fn bundle_executable_preserves_package_layout_and_install_method() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let package = home.path().join("packages/standalone/releases/test");
    let executable = package.join("CodexCLI.app/Contents/MacOS/codex");
    fs::create_dir_all(executable.parent().unwrap())?;
    fs::write(&executable, "")?;
    for directory in [BIN_DIRNAME, RESOURCES_DIRNAME, PATH_DIRNAME] {
        fs::create_dir_all(package.join(directory))?;
    }
    fs::write(
        package.join(PACKAGE_METADATA_FILENAME),
        r#"{"version":"1.2.3"}"#,
    )?;
    let package = canonical_absolute_path(&package).unwrap();
    let bin_dir = package.join(BIN_DIRNAME);
    let resources_dir = package.join(RESOURCES_DIRNAME);
    let path_dir = package.join(PATH_DIRNAME);
    let context = InstallContext::from_exe_with_codex_home(
        /*is_macos*/ true,
        /*current_exe*/ Some(&executable),
        /*method_override*/ None,
        /*codex_home*/ Some(home.path()),
    );
    assert_eq!(
        context,
        InstallContext {
            method: InstallMethod::Standalone {
                release_dir: package.clone(),
                resources_dir: Some(resources_dir.clone()),
                platform: standalone_platform(),
            },
            package_layout: Some(CodexPackageLayout {
                package_dir: package,
                bin_dir,
                resources_dir: Some(resources_dir),
                path_dir: Some(path_dir),
            }),
        }
    );
    Ok(())
}

#[test]
fn local_package_lanes_are_recognized_and_expose_package_resources() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;

    for (package_name, expected_lane) in [
        (LOCAL_CODEX_PACKAGES_DIRNAME, LocalPackageLane::Codex),
        (LOCAL_CDX_DEV_PACKAGES_DIRNAME, LocalPackageLane::CdxDev),
    ] {
        let (executable, mcp_bin, sky_bin) = create_package_release(home.path(), package_name)?;
        let context = InstallContext::from_exe_with_codex_home(
            /*is_macos*/ false,
            /*current_exe*/ Some(&executable),
            /*method_override*/ None,
            /*codex_home*/ Some(home.path()),
        );

        assert_eq!(
            context.local_package_lane_with_codex_home(home.path()),
            Some(expected_lane)
        );
        assert_eq!(
            context.bundled_resource(COMPUTER_USE_MCP_RESOURCE_NAME),
            Some(canonical_absolute_path(&mcp_bin).expect("MCP resource should canonicalize"))
        );
        assert_eq!(
            context.bundled_resource(SKY_RESOURCE_NAME),
            Some(canonical_absolute_path(&sky_bin).expect("Sky resource should canonicalize"))
        );
    }

    Ok(())
}

#[test]
fn standalone_and_generic_packages_do_not_bind_to_a_local_lane() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;

    for package_name in [STANDALONE_PACKAGES_DIRNAME, "generic"] {
        let (executable, _, _) = create_package_release(home.path(), package_name)?;
        let context = InstallContext::from_exe_with_codex_home(
            /*is_macos*/ false,
            /*current_exe*/ Some(&executable),
            /*method_override*/ None,
            /*codex_home*/ Some(home.path()),
        );

        assert_eq!(
            context.local_package_lane_with_codex_home(home.path()),
            None
        );
    }

    Ok(())
}

#[cfg(unix)]
#[test]
fn local_package_lane_follows_a_canonical_selector_symlink() -> std::io::Result<()> {
    let home = tempfile::tempdir()?;
    let (executable, _, _) = create_package_release(home.path(), LOCAL_CODEX_PACKAGES_DIRNAME)?;
    let selector = home.path().join("packages/local-codex/current");
    std::os::unix::fs::symlink(
        executable
            .parent()
            .and_then(Path::parent)
            .expect("package executable should have a release directory"),
        &selector,
    )?;
    let selected_executable = selector.join("bin/codex");
    let context = InstallContext::from_exe_with_codex_home(
        /*is_macos*/ false,
        /*current_exe*/ Some(&selected_executable),
        /*method_override*/ None,
        /*codex_home*/ Some(home.path()),
    );

    assert_eq!(
        context.local_package_lane_with_codex_home(home.path()),
        Some(LocalPackageLane::Codex)
    );

    Ok(())
}

fn create_package_release(
    home: &Path,
    package_name: &str,
) -> std::io::Result<(PathBuf, PathBuf, PathBuf)> {
    let release = home
        .join("packages")
        .join(package_name)
        .join(RELEASES_DIRNAME)
        .join("test");
    let bin_dir = release.join(BIN_DIRNAME);
    let resources_dir = release.join(RESOURCES_DIRNAME);
    let executable = bin_dir.join(if cfg!(windows) { "codex.exe" } else { "codex" });
    let mcp_bin = resources_dir.join(COMPUTER_USE_MCP_RESOURCE_NAME);
    let sky_bin = resources_dir.join(SKY_RESOURCE_NAME);

    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&resources_dir)?;
    fs::write(release.join(PACKAGE_METADATA_FILENAME), "{}")?;
    fs::write(&executable, "")?;
    fs::write(&mcp_bin, "")?;
    fs::write(&sky_bin, "")?;

    Ok((executable, mcp_bin, sky_bin))
}

#[cfg(windows)]
#[test]
fn winget_root_requires_metadata_for_the_actual_executable() -> std::io::Result<()> {
    let temp = tempfile::tempdir()?;
    let package = canonical_absolute_path(temp.path()).unwrap();
    let name = "codex-x86_64-pc-windows-msvc.exe";
    let executable = package.join(name);
    fs::write(&executable, "signed CLI")?;
    for directory in [RESOURCES_DIRNAME, PATH_DIRNAME] {
        fs::create_dir_all(package.join(directory))?;
    }
    assert_eq!(CodexPackageLayout::from_exe(executable.as_path()), None);
    for entrypoint in ["other.exe", "bin/codex.exe", name] {
        fs::write(
            package.join(PACKAGE_METADATA_FILENAME),
            serde_json::json!({"layoutVersion": 1, "entrypoint": entrypoint}).to_string(),
        )?;
        let expected = (entrypoint == name).then(|| CodexPackageLayout {
            package_dir: package.clone(),
            bin_dir: package.clone(),
            resources_dir: Some(package.join(RESOURCES_DIRNAME)),
            path_dir: Some(package.join(PATH_DIRNAME)),
        });
        assert_eq!(CodexPackageLayout::from_exe(executable.as_path()), expected);
    }
    Ok(())
}
