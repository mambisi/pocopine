use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use notify::{Config, ErrorKind, Event, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};

use crate::args::ServeArgs;
use crate::{build, client_modules, config, env, server, stylekit, tailwind};

const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(250);
const CHANGE_QUIET_WINDOW: Duration = Duration::from_millis(350);
const CHANGE_MAX_COALESCE: Duration = Duration::from_secs(4);

pub fn run(args: &ServeArgs) -> Result<()> {
    let project = args.path.canonicalize()?;
    let cfg = config::load(&args.path)?;
    server::check_configured_port_available(&cfg, args.port)?;
    build::wasm(&project, args.release)?;
    client_modules::build(&project, args.release)?;
    build::configured_bins(&project, &cfg, args.release)?;

    // Dev-mode `.env` injection: load every KEY=VALUE pair into a map and
    // hand it to spawn_bin so the server / worker children see them.
    // `pocopine run` (in main.rs::run_project) intentionally does NOT do
    // this — production-shape runs must source their environment from the
    // shell / host secrets store, never from a tracked dev convenience
    // file.
    let dev_env = env::load(&project)?;
    if !dev_env.is_empty() {
        let preview: Vec<String> = dev_env.keys().cloned().collect();
        println!(
            "⚙ loading .env: {} key(s) [{}]",
            preview.len(),
            preview.join(", ")
        );
    }

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
    // Pine Stylekit runs in-process (RFC 092 D2) — no watcher child.
    // The initial compile fails loud; later recompiles ride the src
    // watch tick below.
    let stylekit_on = stylekit::enabled(&cfg, args.stylekit, args.no_stylekit);
    if stylekit_on {
        stylekit::run_once(&project, &cfg, args.stylekit, args.release)?;
    }

    // Start the serving side. In bin mode the child owns its ports + routes.
    // In static mode the CLI owns the socket and runs on a background thread.
    server::check_configured_port_available(&cfg, args.port)?;
    if cfg.bin.is_some() || cfg.worker_bin.is_some() {
        spawn_configured_bins(
            &mut children,
            &project,
            &cfg,
            args.release,
            &dev_env,
            args.port,
        )?;
    } else {
        {
            let serve_path = project.clone();
            let port = args.port.unwrap_or(5243);
            thread::spawn(move || {
                if let Err(e) = server::serve_static(&serve_path, port) {
                    eprintln!("server error: {e}");
                }
            });
        }
    }
    println!(
        "👀 watching {} and package files ({watcher_label})",
        src_dir.display()
    );

    loop {
        if let Some(message) = server::poll_children(&mut children.bins)? {
            break Err(anyhow!("{message}"));
        }
        if let Some(message) = children.poll_tailwind()? {
            break Err(anyhow!("{message}"));
        }
        match rx.recv_timeout(CHILD_POLL_INTERVAL) {
            Ok(change) => {
                let pending = coalesce_changes(&rx, change);

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

                if pending.server {
                    println!("↻ rebuilding server bin…");
                    if let Err(e) = build::configured_bins(&project, &cfg, args.release) {
                        eprintln!("server build failed: {e:#}");
                        continue;
                    }
                    if let Err(e) = restart_configured_bins(
                        &mut children,
                        &project,
                        &cfg,
                        args.release,
                        &dev_env,
                        args.port,
                    ) {
                        eprintln!("server restart failed: {e:#}");
                        continue;
                    }
                }

                if pending.client {
                    println!("↻ rebundling client modules…");
                    if let Err(e) = client_modules::build(&project, args.release) {
                        eprintln!("client module build failed: {e:#}");
                    }
                }

                // `.poco` edits land in src/ (already watched) and force
                // a wasm rebuild; recompile CSS on the same tick so the
                // stylesheet tracks template changes. Never aborts the
                // dev loop — errors keep the last good stylesheet.
                if stylekit_on && pending.wasm {
                    stylekit::recompile_quiet(&project, &cfg);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break Ok(()),
        }
    }
}

fn spawn_configured_bins(
    children: &mut DevChildren,
    project: &Path,
    cfg: &config::PocopineConfig,
    release: bool,
    dev_env: &std::collections::BTreeMap<String, String>,
    override_port: Option<u16>,
) -> Result<()> {
    if let Some(bin) = cfg.bin.as_deref() {
        // The web bin gets PORT = --port override, else the manifest port,
        // so its bound addr matches the port the preflight checked.
        let mut server_env = dev_env.clone();
        if let Some(p) = override_port.or(cfg.port) {
            server_env.insert("PORT".to_string(), p.to_string());
        }
        children.bins.push(server::spawn_bin_with_env(
            project,
            bin,
            release,
            server::BinRole::Server,
            true,
            &server_env,
        )?);
    }
    if let Some(worker) = cfg.worker_bin.as_deref() {
        server::validate_worker_backend_for_separate_process(true)?;
        children.bins.push(server::spawn_bin_with_env(
            project,
            worker,
            release,
            server::BinRole::Worker,
            true,
            dev_env,
        )?);
    }
    Ok(())
}

fn restart_configured_bins(
    children: &mut DevChildren,
    project: &Path,
    cfg: &config::PocopineConfig,
    release: bool,
    dev_env: &std::collections::BTreeMap<String, String>,
    override_port: Option<u16>,
) -> Result<()> {
    for child in children.bins.drain(..) {
        child.kill();
    }
    spawn_configured_bins(children, project, cfg, release, dev_env, override_port)
}

fn coalesce_changes(rx: &Receiver<Change>, first: Change) -> Change {
    let mut pending = first;
    let deadline = Instant::now() + CHANGE_MAX_COALESCE;

    loop {
        while let Ok(change) = rx.try_recv() {
            pending.merge(change);
        }

        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let timeout = CHANGE_QUIET_WINDOW.min(deadline - now);
        match rx.recv_timeout(timeout) {
            Ok(change) => pending.merge(change),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
        }
    }

    pending
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
    // RFC-100 — asset edits must rebuild wasm + server bins: the
    // rebuild re-exports POCOPINE_ASSETS_FINGERPRINT, which
    // re-expands `asset!` with the fresh content hash. Only watched
    // when the directory exists at dev start; a dir created
    // mid-session needs a dev restart (documented in RFC-100 §6).
    let assets_dir = project.join("assets");
    if assets_dir.is_dir() {
        watcher.watch(&assets_dir, RecursiveMode::Recursive)?;
    }
    Ok(())
}

fn watcher_callback(
    project: std::path::PathBuf,
    tx: Sender<Change>,
) -> impl FnMut(notify::Result<Event>) {
    move |res: notify::Result<Event>| {
        if let Ok(ev) = res {
            use notify::EventKind::*;
            if matches!(ev.kind, Modify(_) | Create(_) | Remove(_))
                && let Some(change) = Change::from_event(&project, &ev)
            {
                let _ = tx.send(change);
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

impl DevChildren {
    fn poll_tailwind(&mut self) -> Result<Option<String>> {
        let Some(child) = self.tailwind.as_mut() else {
            return Ok(None);
        };
        child.poll()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Change {
    wasm: bool,
    server: bool,
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
        // RFC-100 — anything under assets/ rebuilds both targets so
        // every `asset!` expansion (wasm or server bin) re-hashes.
        if path.starts_with(project.join("assets")) {
            return Some(Self {
                wasm: true,
                server: true,
                ..Self::default()
            });
        }

        let src = project.join("src");
        if path.starts_with(&src) {
            if is_client_module_path(path) || is_unsupported_client_module_path(path) {
                return Some(Self {
                    wasm: is_client_module_path(path),
                    client: true,
                    ..Self::default()
                });
            }
            if is_client_module_support_path(path) {
                return Some(Self {
                    client: true,
                    ..Self::default()
                });
            }
            return Some(Self {
                wasm: true,
                server: is_rust_source_path(path),
                ..Self::default()
            });
        }

        if path.parent() == Some(project) {
            let name = path.file_name().and_then(|name| name.to_str())?;
            if is_package_manifest(name) {
                return Some(Self {
                    client: true,
                    install: true,
                    ..Self::default()
                });
            }
            if is_package_lockfile(name) {
                return Some(Self {
                    client: true,
                    ..Self::default()
                });
            }
        }

        None
    }

    fn merge(&mut self, other: Self) {
        self.wasm |= other.wasm;
        self.server |= other.server;
        self.client |= other.client;
        self.install |= other.install;
    }
}

fn is_rust_source_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
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

fn is_client_module_support_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.ends_with(".ts") || name.ends_with(".d.ts")
}

fn is_package_manifest(name: &str) -> bool {
    name == "package.json"
}

fn is_package_lockfile(name: &str) -> bool {
    matches!(
        name,
        "pnpm-lock.yaml" | "package-lock.json" | "yarn.lock" | "bun.lockb" | "bun.lock"
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
    fn asset_edits_rebuild_wasm_and_server_bin() {
        // RFC-100 — the rebuild re-exports the assets fingerprint,
        // which re-expands `asset!` everywhere it appears.
        assert_eq!(
            Change::from_path(&project(), &project().join("assets/blog/clip.webm")),
            Some(Change {
                wasm: true,
                server: true,
                ..Change::default()
            })
        );
    }

    #[test]
    fn rust_source_rebuilds_wasm_and_server_bin() {
        assert_eq!(
            Change::from_path(&project(), &project().join("src/lib.rs")),
            Some(Change {
                wasm: true,
                server: true,
                ..Change::default()
            })
        );
    }

    #[test]
    fn poco_templates_rebuild_wasm_without_server_bin() {
        assert_eq!(
            Change::from_path(&project(), &project().join("src/App.poco")),
            Some(Change {
                wasm: true,
                ..Change::default()
            })
        );
    }

    #[test]
    fn typescript_support_files_rebundle_client_modules_only() {
        assert_eq!(
            Change::from_path(&project(), &project().join("src/firebase/bindings.ts")),
            Some(Change {
                client: true,
                ..Change::default()
            })
        );
    }

    #[test]
    fn package_json_installs_and_rebundles_client_modules() {
        assert_eq!(
            Change::from_path(&project(), &project().join("package.json")),
            Some(Change {
                client: true,
                install: true,
                ..Change::default()
            })
        );
    }

    #[test]
    fn package_lock_rebundles_without_installing_again() {
        assert_eq!(
            Change::from_path(&project(), &project().join("package-lock.json")),
            Some(Change {
                client: true,
                ..Change::default()
            })
        );
    }
}
