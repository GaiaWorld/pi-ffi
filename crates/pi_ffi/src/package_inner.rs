use super::*;
use pi_result::{Error, ErrorKind};
use std::path::Component;

pub(super) unsafe fn package_from_verified_build(
    root: PathBuf,
    manifest: BuildManifest,
) -> Result<PluginPackage> {
    let target_matches = match manifest.target {
        Target::LinuxX86_64Gnu => cfg!(all(
            target_os = "linux",
            target_env = "gnu",
            target_arch = "x86_64"
        )),
        Target::WindowsX86_64Msvc => cfg!(all(
            target_os = "windows",
            target_env = "msvc",
            target_arch = "x86_64"
        )),
    };
    if !target_matches {
        return Err(Error::new(ErrorKind::Unsupported)
            .attach(format!("目标平台（target platform） {:?}", manifest.target)));
    }
    if manifest.uuid == [0; 16] || manifest.allocator.trim().is_empty() {
        return Err(Error::new(ErrorKind::InvalidInput)
            .attach("插件身份（plugin identity）或分配器（allocator）声明无效"));
    }

    let root = root.canonicalize().map_err(|cause| {
        Error::new(ErrorKind::Io)
            .attach(cause)
            .attach(pi_result::attachment::PathAttachment(root.clone()))
    })?;
    if !root.is_dir() {
        return Err(
            Error::new(ErrorKind::InvalidInput).attach(pi_result::attachment::PathAttachment(root))
        );
    }
    let main = verified_path(&root, &manifest.library.path)?;
    for (index, dependency) in manifest.dependencies.iter().enumerate() {
        if dependency.load_name.is_empty() || dependency.load_name.contains(['\0', '/', '\\']) {
            return Err(Error::new(ErrorKind::InvalidInput).attach(dependency.load_name.clone()));
        }
        if manifest.dependencies[..index]
            .iter()
            .any(|previous| match manifest.target {
                Target::LinuxX86_64Gnu => previous.load_name == dependency.load_name,
                Target::WindowsX86_64Msvc => previous
                    .load_name
                    .eq_ignore_ascii_case(&dependency.load_name),
            })
        {
            return Err(Error::new(ErrorKind::Conflict).attach(dependency.load_name.clone()));
        }
        let file = verified_path(&root, &dependency.file.path)?;
        if file == main {
            return Err(
                Error::new(ErrorKind::Conflict).attach(pi_result::attachment::PathAttachment(file))
            );
        }
    }

    Ok(PluginPackage {
        root: Arc::from(root),
        manifest: Arc::new(manifest),
    })
}

pub(super) fn package_root(package: &PluginPackage) -> &Path {
    &package.root
}

pub(super) fn package_manifest(package: &PluginPackage) -> &BuildManifest {
    &package.manifest
}

fn verified_path(root: &Path, path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() || path.components().any(|part| part == Component::ParentDir) {
        return Err(Error::new(ErrorKind::InvalidInput)
            .attach(pi_result::attachment::PathAttachment(path.to_owned())));
    }
    let candidate = root.join(path);
    let actual = candidate.canonicalize().map_err(|cause| {
        Error::new(ErrorKind::Io)
            .attach(cause)
            .attach(pi_result::attachment::PathAttachment(candidate))
    })?;
    if !actual.starts_with(root) || !actual.is_file() {
        return Err(Error::new(ErrorKind::InvalidInput)
            .attach(pi_result::attachment::PathAttachment(actual)));
    }
    Ok(actual)
}
