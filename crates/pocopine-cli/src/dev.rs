use std::path::Path;
use std::sync::mpsc::channel;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use crate::args::ServeArgs;
use crate::{build, client_modules, config, server, tailwind};

pub fn run(args: &ServeArgs) -> Result<()> {
    let project = args.path.canonicalize()?;
    let cfg = config::load(&args.path)?;
    build::wasm(&project, args.release)?;
    client_modules::build(&project, args.release)?;
    build::configured_bins(&project, &cfg, args.release)?;

    // Kick off Tailwind in watch mode before serving so the first page
    // load already sees compiled CSS.
    let tailwind_child = if let Some(tw) = cfg.tailwind.as_ref() {
        tailwind::run_once(&project, tw, args.release)?;
        Some(tailwind::spawn_watch(&project, tw)?)
    } else {
        None
    };

    // Start the serving side. In bin mode the child owns its ports + routes.
    // In static mode the CLI owns the socket and runs on a background thread.
    let mut bin_children: Vec<server::BinChild> = Vec::new();
    match cfg.bin.as_deref() {
        Some(bin) => bin_children.push(server::spawn_bin(
            &project,
            bin,
            args.release,
            server::BinRole::Server,
            true,
        )?),
        None => {
            let serve_path = project.clone();
            let port = args.port;
            thread::spawn(move || {
                if let Err(e) = server::serve_static(&serve_path, port) {
                    eprintln!("server error: {e}");
                }
            });
        }
    }
    if let Some(worker) = cfg.worker_bin.as_deref() {
        server::validate_worker_backend_for_separate_process(true)?;
        bin_children.push(server::spawn_bin(
            &project,
            worker,
            args.release,
            server::BinRole::Worker,
            true,
        )?);
    }

    let (tx, rx) = channel::<Change>();
    let tx_w = tx.clone();
    let project_for_watch = project.clone();
    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res: notify::Result<Event>| {
            if let Ok(ev) = res {
                use notify::EventKind::*;
                if matches!(ev.kind, Modify(_) | Create(_) | Remove(_)) {
                    if let Some(change) = Change::from_event(&project_for_watch, &ev) {
                        let _ = tx_w.send(change);
                    }
                }
            }
        })?;
    let src_dir = project.join("src");
    watcher.watch(&src_dir, RecursiveMode::Recursive)?;
    watcher.watch(&project, RecursiveMode::NonRecursive)?;
    println!("👀 watching {} and package files", src_dir.display());

    let result = loop {
        if let Some(message) = server::poll_children(&mut bin_children)? {
            break Err(anyhow!("{message}"));
        }
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(change) => {
                let mut pending = change;
                while let Ok(change) = rx.try_recv() {
                    pending.merge(change);
                }

                if pending.install {
                    println!("↻ installing client dependencies…");
                    if let Err(e) = client_modules::install(&project) {
                        eprintln!("client dependency install failed: {e:#}");
                        continue;
                    }
                }

                if pending.wasm {
                    println!("↻ rebuilding wasm…");
                    if let Err(e) = build::wasm(&project, args.release) {
                        eprintln!("build failed: {e:#}");
                        continue;
                    }
                }

                if pending.client {
                    println!("↻ rebundling client modules…");
                    if let Err(e) = client_modules::build(&project, args.release) {
                        eprintln!("client module build failed: {e:#}");
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
        }
    };

    for child in bin_children {
        child.kill();
    }
    if let Some(child) = tailwind_child {
        child.kill();
    }
    result
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Change {
    wasm: bool,
    client: bool,
    install: bool,
}

impl Change {
    fn from_event(project: &Path, event: &Event) -> Option<Self> {
        event
            .paths
            .iter()
            .filter_map(|path| Self::from_path(project, path))
            .reduce(|mut pending, change| {
                pending.merge(change);
                pending
            })
    }

    fn from_path(project: &Path, path: &Path) -> Option<Self> {
        let src = project.join("src");
        if path.starts_with(&src) {
            if is_client_module_path(path) || is_unsupported_client_module_path(path) {
                return Some(Self {
                    client: true,
                    ..Self::default()
                });
            }
            return Some(Self {
                wasm: true,
                ..Self::default()
            });
        }

        if path.parent() == Some(project) {
            let name = path.file_name().and_then(|name| name.to_str())?;
            if is_package_file(name) {
                return Some(Self {
                    client: true,
                    install: true,
                    ..Self::default()
                });
            }
        }

        None
    }

    fn merge(&mut self, other: Self) {
        self.wasm |= other.wasm;
        self.client |= other.client;
        self.install |= other.install;
    }
}

fn is_client_module_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.ends_with(".client.js") || name.ends_with(".client.ts")
}

fn is_unsupported_client_module_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.ends_with(".client.jsx") || name.ends_with(".client.tsx")
}

fn is_package_file(name: &str) -> bool {
    matches!(
        name,
        "package.json"
            | "pnpm-lock.yaml"
            | "package-lock.json"
            | "yarn.lock"
            | "bun.lockb"
            | "bun.lock"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn project() -> PathBuf {
        PathBuf::from("/tmp/pocopine-dev-watch")
    }

    #[test]
    fn client_modules_rebundle_without_wasm() {
        assert_eq!(
            Change::from_path(&project(), &project().join("src/FirebaseAuth.client.ts")),
            Some(Change {
                client: true,
                ..Change::default()
            })
        );
    }

    #[test]
    fn rust_source_rebuilds_only_wasm() {
        assert_eq!(
            Change::from_path(&project(), &project().join("src/lib.rs")),
            Some(Change {
                wasm: true,
                ..Change::default()
            })
        );
    }

    #[test]
    fn package_files_install_and_rebundle_client_modules() {
        assert_eq!(
            Change::from_path(&project(), &project().join("package.json")),
            Some(Change {
                client: true,
                install: true,
                ..Change::default()
            })
        );
    }
}
