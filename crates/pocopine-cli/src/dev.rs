use std::path::Path;
use std::sync::mpsc::{channel, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use notify::{Config, ErrorKind, Event, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};

use crate::args::ServeArgs;
use crate::{build, client_modules, config, server, tailwind};

pub fn run(args: &ServeArgs) -> Result<()> {
    let project = args.path.canonicalize()?;
    let cfg = config::load(&args.path)?;
    build::wasm(&project, args.release)?;
    client_modules::build(&project, args.release)?;
    build::configured_bins(&project, &cfg, args.release)?;

    let (tx, rx) = channel::<Change>();
    let src_dir = project.join("src");
    let (_watcher, watcher_label) = start_watcher(&project, &src_dir, tx)?;

    // Kick off Tailwind in watch mode before serving so the first page
    // load already sees compiled CSS.
    let mut children = DevChildren::default();
    if let Some(tw) = cfg.tailwind.as_ref() {
        tailwind::run_once(&project, tw, args.release)?;
        children.tailwind = Some(tailwind::spawn_watch(&project, tw)?);
    }

    // Start the serving side. In bin mode the child owns its ports + routes.
    // In static mode the CLI owns the socket and runs on a background thread.
    match cfg.bin.as_deref() {
        Some(bin) => children.bins.push(server::spawn_bin(
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
        children.bins.push(server::spawn_bin(
            &project,
            worker,
            args.release,
            server::BinRole::Worker,
            true,
        )?);
    }
    println!(
        "👀 watching {} and package files ({watcher_label})",
        src_dir.display()
    );

    loop {
        if let Some(message) = server::poll_children(&mut children.bins)? {
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
    }
}

enum DevWatcher {
    Native(RecommendedWatcher),
    Poll(PollWatcher),
}

impl DevWatcher {
    fn watch(&mut self, path: &Path, mode: RecursiveMode) -> notify::Result<()> {
        match self {
            Self::Native(watcher) => watcher.watch(path, mode),
            Self::Poll(watcher) => watcher.watch(path, mode),
        }
    }
}

fn start_watcher(
    project: &Path,
    src_dir: &Path,
    tx: Sender<Change>,
) -> Result<(DevWatcher, &'static str)> {
    match start_native_watcher(project, src_dir, tx.clone()) {
        Ok(watcher) => Ok((DevWatcher::Native(watcher), "native watcher")),
        Err(err) if should_fallback_to_polling(&err) => {
            eprintln!(
                "⚠ native file watcher limit reached; falling back to polling every 2s. \
                 Raise fs.inotify.max_user_watches later for faster dev reloads."
            );
            let watcher = start_poll_watcher(project, src_dir, tx)
                .context("create polling file watcher fallback")?;
            Ok((DevWatcher::Poll(watcher), "polling watcher"))
        }
        Err(err) => Err(err).context("create file watcher"),
    }
}

fn start_native_watcher(
    project: &Path,
    src_dir: &Path,
    tx: Sender<Change>,
) -> notify::Result<RecommendedWatcher> {
    let mut watcher = DevWatcher::Native(notify::recommended_watcher(watcher_callback(
        project.to_path_buf(),
        tx,
    ))?);
    watch_dev_paths(&mut watcher, project, src_dir)?;
    let DevWatcher::Native(watcher) = watcher else {
        unreachable!("created native watcher")
    };
    Ok(watcher)
}

fn start_poll_watcher(
    project: &Path,
    src_dir: &Path,
    tx: Sender<Change>,
) -> notify::Result<PollWatcher> {
    let config = Config::default().with_poll_interval(Duration::from_secs(2));
    let mut watcher = DevWatcher::Poll(PollWatcher::new(
        watcher_callback(project.to_path_buf(), tx),
        config,
    )?);
    watch_dev_paths(&mut watcher, project, src_dir)?;
    let DevWatcher::Poll(watcher) = watcher else {
        unreachable!("created polling watcher")
    };
    Ok(watcher)
}

fn watch_dev_paths(watcher: &mut DevWatcher, project: &Path, src_dir: &Path) -> notify::Result<()> {
    watcher.watch(src_dir, RecursiveMode::Recursive)?;
    watcher.watch(project, RecursiveMode::NonRecursive)?;
    Ok(())
}

fn watcher_callback(
    project: std::path::PathBuf,
    tx: Sender<Change>,
) -> impl FnMut(notify::Result<Event>) {
    move |res: notify::Result<Event>| {
        if let Ok(ev) = res {
            use notify::EventKind::*;
            if matches!(ev.kind, Modify(_) | Create(_) | Remove(_)) {
                if let Some(change) = Change::from_event(&project, &ev) {
                    let _ = tx.send(change);
                }
            }
        }
    }
}

fn should_fallback_to_polling(err: &notify::Error) -> bool {
    matches!(err.kind, ErrorKind::MaxFilesWatch)
}

#[derive(Default)]
struct DevChildren {
    bins: Vec<server::BinChild>,
    tailwind: Option<tailwind::TailwindChild>,
}

impl Drop for DevChildren {
    fn drop(&mut self) {
        for child in self.bins.drain(..) {
            child.kill();
        }
        if let Some(child) = self.tailwind.take() {
            child.kill();
        }
    }
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
                    wasm: is_client_module_path(path),
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
    name.ends_with(".client.ts")
}

fn is_unsupported_client_module_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.ends_with(".client.js") || name.ends_with(".client.jsx") || name.ends_with(".client.tsx")
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
    fn typed_client_modules_rebuild_wasm_and_rebundle() {
        assert_eq!(
            Change::from_path(&project(), &project().join("src/FirebaseAuth.client.ts")),
            Some(Change {
                wasm: true,
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
