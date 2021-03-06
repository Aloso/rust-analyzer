//! FIXME: write short doc here

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cargo_metadata::{CargoOpt, Message, MetadataCommand, PackageId};
use ra_arena::{impl_arena_id, Arena, RawId};
use ra_cargo_watch::run_cargo;
use ra_db::Edition;
use rustc_hash::FxHashMap;
use serde::Deserialize;

/// `CargoWorkspace` represents the logical structure of, well, a Cargo
/// workspace. It pretty closely mirrors `cargo metadata` output.
///
/// Note that internally, rust analyzer uses a different structure:
/// `CrateGraph`. `CrateGraph` is lower-level: it knows only about the crates,
/// while this knows about `Packages` & `Targets`: purely cargo-related
/// concepts.
#[derive(Debug, Clone)]
pub struct CargoWorkspace {
    packages: Arena<Package, PackageData>,
    targets: Arena<Target, TargetData>,
    workspace_root: PathBuf,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct CargoFeatures {
    /// Do not activate the `default` feature.
    pub no_default_features: bool,

    /// Activate all available features
    pub all_features: bool,

    /// List of features to activate.
    /// This will be ignored if `cargo_all_features` is true.
    pub features: Vec<String>,

    /// Runs cargo check on launch to figure out the correct values of OUT_DIR
    pub load_out_dirs_from_check: bool,
}

impl Default for CargoFeatures {
    fn default() -> Self {
        CargoFeatures {
            no_default_features: false,
            all_features: true,
            features: Vec::new(),
            load_out_dirs_from_check: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Package(RawId);
impl_arena_id!(Package);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Target(RawId);
impl_arena_id!(Target);

#[derive(Debug, Clone)]
struct PackageData {
    name: String,
    manifest: PathBuf,
    targets: Vec<Target>,
    is_member: bool,
    dependencies: Vec<PackageDependency>,
    edition: Edition,
    features: Vec<String>,
    out_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct PackageDependency {
    pub pkg: Package,
    pub name: String,
}

#[derive(Debug, Clone)]
struct TargetData {
    pkg: Package,
    name: String,
    root: PathBuf,
    kind: TargetKind,
    is_proc_macro: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Bin,
    /// Any kind of Cargo lib crate-type (dylib, rlib, proc-macro, ...).
    Lib,
    Example,
    Test,
    Bench,
    Other,
}

impl TargetKind {
    fn new(kinds: &[String]) -> TargetKind {
        for kind in kinds {
            return match kind.as_str() {
                "bin" => TargetKind::Bin,
                "test" => TargetKind::Test,
                "bench" => TargetKind::Bench,
                "example" => TargetKind::Example,
                "proc-macro" => TargetKind::Lib,
                _ if kind.contains("lib") => TargetKind::Lib,
                _ => continue,
            };
        }
        TargetKind::Other
    }
}

impl Package {
    pub fn name(self, ws: &CargoWorkspace) -> &str {
        ws.packages[self].name.as_str()
    }
    pub fn root(self, ws: &CargoWorkspace) -> &Path {
        ws.packages[self].manifest.parent().unwrap()
    }
    pub fn edition(self, ws: &CargoWorkspace) -> Edition {
        ws.packages[self].edition
    }
    pub fn features(self, ws: &CargoWorkspace) -> &[String] {
        &ws.packages[self].features
    }
    pub fn targets<'a>(self, ws: &'a CargoWorkspace) -> impl Iterator<Item = Target> + 'a {
        ws.packages[self].targets.iter().cloned()
    }
    #[allow(unused)]
    pub fn is_member(self, ws: &CargoWorkspace) -> bool {
        ws.packages[self].is_member
    }
    pub fn dependencies<'a>(
        self,
        ws: &'a CargoWorkspace,
    ) -> impl Iterator<Item = &'a PackageDependency> + 'a {
        ws.packages[self].dependencies.iter()
    }
    pub fn out_dir(self, ws: &CargoWorkspace) -> Option<&Path> {
        ws.packages[self].out_dir.as_ref().map(PathBuf::as_path)
    }
}

impl Target {
    pub fn package(self, ws: &CargoWorkspace) -> Package {
        ws.targets[self].pkg
    }
    pub fn name(self, ws: &CargoWorkspace) -> &str {
        ws.targets[self].name.as_str()
    }
    pub fn root(self, ws: &CargoWorkspace) -> &Path {
        ws.targets[self].root.as_path()
    }
    pub fn kind(self, ws: &CargoWorkspace) -> TargetKind {
        ws.targets[self].kind
    }
    pub fn is_proc_macro(self, ws: &CargoWorkspace) -> bool {
        ws.targets[self].is_proc_macro
    }
}

impl CargoWorkspace {
    pub fn from_cargo_metadata(
        cargo_toml: &Path,
        cargo_features: &CargoFeatures,
    ) -> Result<CargoWorkspace> {
        let mut meta = MetadataCommand::new();
        meta.manifest_path(cargo_toml);
        if cargo_features.all_features {
            meta.features(CargoOpt::AllFeatures);
        } else if cargo_features.no_default_features {
            // FIXME: `NoDefaultFeatures` is mutual exclusive with `SomeFeatures`
            // https://github.com/oli-obk/cargo_metadata/issues/79
            meta.features(CargoOpt::NoDefaultFeatures);
        } else if !cargo_features.features.is_empty() {
            meta.features(CargoOpt::SomeFeatures(cargo_features.features.clone()));
        }
        if let Some(parent) = cargo_toml.parent() {
            meta.current_dir(parent);
        }
        let meta = meta.exec().with_context(|| {
            format!("Failed to run `cargo metadata --manifest-path {}`", cargo_toml.display())
        })?;

        let mut out_dir_by_id = FxHashMap::default();
        if cargo_features.load_out_dirs_from_check {
            out_dir_by_id = load_out_dirs(cargo_toml, cargo_features);
        }

        let mut pkg_by_id = FxHashMap::default();
        let mut packages = Arena::default();
        let mut targets = Arena::default();

        let ws_members = &meta.workspace_members;

        for meta_pkg in meta.packages {
            let cargo_metadata::Package { id, edition, name, manifest_path, .. } = meta_pkg;
            let is_member = ws_members.contains(&id);
            let edition = edition
                .parse::<Edition>()
                .with_context(|| format!("Failed to parse edition {}", edition))?;
            let pkg = packages.alloc(PackageData {
                name,
                manifest: manifest_path,
                targets: Vec::new(),
                is_member,
                edition,
                dependencies: Vec::new(),
                features: Vec::new(),
                out_dir: out_dir_by_id.get(&id).cloned(),
            });
            let pkg_data = &mut packages[pkg];
            pkg_by_id.insert(id, pkg);
            for meta_tgt in meta_pkg.targets {
                let is_proc_macro = meta_tgt.kind.as_slice() == ["proc-macro"];
                let tgt = targets.alloc(TargetData {
                    pkg,
                    name: meta_tgt.name,
                    root: meta_tgt.src_path.clone(),
                    kind: TargetKind::new(meta_tgt.kind.as_slice()),
                    is_proc_macro,
                });
                pkg_data.targets.push(tgt);
            }
        }
        let resolve = meta.resolve.expect("metadata executed with deps");
        for node in resolve.nodes {
            let source = match pkg_by_id.get(&node.id) {
                Some(&src) => src,
                // FIXME: replace this and a similar branch below with `.unwrap`, once
                // https://github.com/rust-lang/cargo/issues/7841
                // is fixed and hits stable (around 1.43-is probably?).
                None => {
                    log::error!("Node id do not match in cargo metadata, ignoring {}", node.id);
                    continue;
                }
            };
            for dep_node in node.deps {
                let pkg = match pkg_by_id.get(&dep_node.pkg) {
                    Some(&pkg) => pkg,
                    None => {
                        log::error!(
                            "Dep node id do not match in cargo metadata, ignoring {}",
                            dep_node.pkg
                        );
                        continue;
                    }
                };
                let dep = PackageDependency { name: dep_node.name, pkg };
                packages[source].dependencies.push(dep);
            }
            packages[source].features.extend(node.features);
        }

        Ok(CargoWorkspace { packages, targets, workspace_root: meta.workspace_root })
    }

    pub fn packages<'a>(&'a self) -> impl Iterator<Item = Package> + ExactSizeIterator + 'a {
        self.packages.iter().map(|(id, _pkg)| id)
    }

    pub fn target_by_root(&self, root: &Path) -> Option<Target> {
        self.packages().filter_map(|pkg| pkg.targets(self).find(|it| it.root(self) == root)).next()
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }
}

pub fn load_out_dirs(
    cargo_toml: &Path,
    cargo_features: &CargoFeatures,
) -> FxHashMap<PackageId, PathBuf> {
    let mut args: Vec<String> = vec![
        "check".to_string(),
        "--message-format=json".to_string(),
        "--manifest-path".to_string(),
        format!("{}", cargo_toml.display()),
    ];

    if cargo_features.all_features {
        args.push("--all-features".to_string());
    } else if cargo_features.no_default_features {
        // FIXME: `NoDefaultFeatures` is mutual exclusive with `SomeFeatures`
        // https://github.com/oli-obk/cargo_metadata/issues/79
        args.push("--no-default-features".to_string());
    } else if !cargo_features.features.is_empty() {
        for feature in &cargo_features.features {
            args.push(feature.clone());
        }
    }

    let mut res = FxHashMap::default();
    let mut child = run_cargo(&args, cargo_toml.parent(), &mut |message| {
        match message {
            Message::BuildScriptExecuted(message) => {
                let package_id = message.package_id;
                let out_dir = message.out_dir;
                res.insert(package_id, out_dir);
            }

            Message::CompilerArtifact(_) => (),
            Message::CompilerMessage(_) => (),
            Message::Unknown => (),
        }
        true
    });

    let _ = child.wait();
    res
}
