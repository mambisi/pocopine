use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use pocopine_locale::{
    Locale,
    server::{
        CatalogSource, ProjectDiscovery, ReferenceKind, Severity, SourceMessage, SourceMessages,
        Span, export_xliff, import_xliff, parse_messages,
    },
};
use serde::Serialize;

use crate::args::{LocaleArgs, LocaleCmd};

type Catalogs = BTreeMap<Locale, SourceMessages>;

pub fn run(args: &LocaleArgs) -> Result<()> {
    let project = args.path.canonicalize()?;
    let allow_missing = matches!(
        args.cmd,
        LocaleCmd::Extract | LocaleCmd::Merge { .. } | LocaleCmd::Import { .. }
    );
    let discovery = super::inspect(&project, args.release, allow_missing)?;
    if discovery.has_errors() {
        super::report(&discovery, &discovery.extracted.diagnostics);
        bail!("locale source discovery failed");
    }
    let config = discovery
        .config
        .as_ref()
        .context("locale configuration is missing")?;
    let mut catalogs = catalogs(&discovery)?;
    match &args.cmd {
        LocaleCmd::Check { deny_warnings } => {
            let compiled = discovery.compile();
            super::report(&discovery, &compiled.diagnostics);
            if compiled.has_errors() || (*deny_warnings && !compiled.diagnostics.is_empty()) {
                bail!("locale check failed");
            }
            println!(
                "✓ {} used messages across {} locales",
                compiled.messages.len(),
                config.locales.len()
            );
        }
        LocaleCmd::Extract => extract(&project, &discovery, &mut catalogs)?,
        LocaleCmd::Stats { json } => stats(&discovery, &catalogs, *json)?,
        LocaleCmd::Merge { locale, input } => {
            let locale = configured_locale(&discovery, locale)?;
            let text = read_text(input)?;
            let updates = parse_messages(&text, 0).map_err(|e| anyhow!(e.message))?;
            merge(&project, &discovery, &mut catalogs, &locale, updates)?;
        }
        LocaleCmd::Export { locale, output, .. } => {
            check(&discovery)?;
            let locale = configured_locale(&discovery, locale)?;
            let xml = export_xliff(
                &config.default,
                &locale,
                &catalogs[&config.default],
                &catalogs[&locale],
                &locations(&project, &discovery),
            )
            .map_err(|e| anyhow!(e))?;
            super::write(output, xml.as_bytes())?;
            println!("✓ wrote {}", output.display());
        }
        LocaleCmd::Import { input } => {
            let imported = import_xliff(&read_text(input)?).map_err(|e| anyhow!(e))?;
            let (locale, updates) = imported_updates(&discovery, &catalogs, imported)?;
            merge(&project, &discovery, &mut catalogs, &locale, updates)?;
        }
    }
    Ok(())
}

fn imported_updates(
    discovery: &ProjectDiscovery,
    catalogs: &Catalogs,
    imported: pocopine_locale::server::XliffDocument,
) -> Result<(Locale, SourceMessages)> {
    let config = discovery
        .config
        .as_ref()
        .context("locale configuration is missing")?;
    if imported.source_locale != config.default {
        bail!(
            "XLIFF source language {} does not match default {}",
            imported.source_locale,
            config.default
        );
    }
    let locale = configured_locale(discovery, imported.target_locale.as_str())?;
    let defaults = catalogs
        .get(&config.default)
        .context("default catalog is missing")?;
    let mut updates = SourceMessages::new();
    for (key, unit) in imported.units {
        let current = defaults
            .get(&key)
            .with_context(|| format!("XLIFF key {key} is absent from the default catalog"))?;
        if current.text != unit.source {
            bail!(
                "default message {key} changed since XLIFF export; export the current source before importing"
            );
        }
        if let Some(text) = unit.target {
            updates.insert(
                key,
                SourceMessage {
                    text,
                    span: Span::UNKNOWN,
                },
            );
        }
    }
    Ok((locale, updates))
}

#[cfg(test)]
mod tests;

pub(super) fn read_text(path: &Path) -> Result<String> {
    let mut text = String::new();
    File::open(path)
        .with_context(|| format!("read {}", path.display()))?
        .take(16 * 1024 * 1024 + 1)
        .read_to_string(&mut text)?;
    if text.len() > 16 * 1024 * 1024 {
        bail!("locale input exceeds 16 MiB");
    }
    Ok(text)
}

fn catalogs(discovery: &ProjectDiscovery) -> Result<Catalogs> {
    let config = discovery
        .config
        .as_ref()
        .context("locale configuration is missing")?;
    let mut catalogs: Catalogs = config
        .locales
        .iter()
        .map(|locale| (locale.clone(), BTreeMap::new()))
        .collect();
    for catalog in &discovery.catalogs {
        match parse_messages(&catalog.source, catalog.file) {
            Ok(messages) => {
                catalogs.insert(catalog.locale.clone(), messages);
            }
            Err(error) => {
                super::report(discovery, std::slice::from_ref(&error));
                bail!("invalid source catalog");
            }
        }
    }
    Ok(catalogs)
}

fn configured_locale(discovery: &ProjectDiscovery, tag: &str) -> Result<Locale> {
    let locale: Locale = tag.parse()?;
    if !discovery
        .config
        .as_ref()
        .context("locale configuration is missing")?
        .locales
        .contains(&locale)
    {
        bail!("locale {locale} is not configured");
    }
    Ok(locale)
}

fn check(discovery: &ProjectDiscovery) -> Result<pocopine_locale::server::Compilation> {
    let compiled = discovery.compile();
    super::report(discovery, &compiled.diagnostics);
    if compiled.has_errors() {
        bail!("locale validation failed");
    }
    Ok(compiled)
}

fn locations(project: &Path, discovery: &ProjectDiscovery) -> BTreeMap<String, Vec<String>> {
    let mut locations: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for reference in &discovery.extracted.references {
        let Some(file) = discovery.files.get(reference.span.file as usize) else {
            continue;
        };
        let Some(before) = file.source.get(..reference.span.start as usize) else {
            continue;
        };
        let line = before.bytes().filter(|b| *b == b'\n').count() + 1;
        let column = before
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .chars()
            .count()
            + 1;
        let path = file.path.strip_prefix(project).unwrap_or(&file.path);
        locations
            .entry(reference.key.clone())
            .or_default()
            .insert(format!("{}:{line}:{column}", path.display()));
    }
    locations
        .into_iter()
        .map(|(key, locations)| (key, locations.into_iter().collect()))
        .collect()
}

fn json(messages: &SourceMessages) -> Result<Vec<u8>> {
    let sorted: BTreeMap<_, _> = messages
        .iter()
        .map(|(key, value)| (key, &value.text))
        .collect();
    let mut bytes = serde_json::to_vec_pretty(&sorted)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn extract(project: &Path, discovery: &ProjectDiscovery, catalogs: &mut Catalogs) -> Result<()> {
    let config = discovery
        .config
        .as_ref()
        .context("locale configuration is missing")?;
    let defaults = catalogs
        .get_mut(&config.default)
        .context("default locale is missing")?;
    let references = &discovery.extracted.references;
    let mut added = 0;
    for reference in references {
        let prefix = format!("{}.", reference.key);
        if matches!(reference.kind, ReferenceKind::RustImport)
            && (reference.key.split('.').count() < 2
                || defaults.keys().any(|key| key.starts_with(&prefix))
                || references
                    .iter()
                    .any(|other| other.key.starts_with(&prefix)))
        {
            continue;
        }
        if !defaults.contains_key(&reference.key) {
            defaults.insert(
                reference.key.clone(),
                SourceMessage {
                    text: String::new(),
                    span: Span::UNKNOWN,
                },
            );
            added += 1;
        }
    }
    // Empty skeleton values are deliberately visible authoring work. Do not
    // invent argument names/types or copy a source language into translations.
    for (locale, messages) in catalogs.iter() {
        let path = catalog_path(project, locale);
        if locale == &config.default || !path.exists() {
            super::write(&path, &json(messages)?)?;
        }
    }
    let notes = project
        .join("locales")
        .join(format!("{}.sources.json", config.default));
    super::write(
        &notes,
        &serde_json::to_vec_pretty(&locations(project, discovery))?,
    )?;
    println!(
        "✓ added {added} keys; source locations: {}",
        notes.display()
    );
    if added > 0 {
        println!("Fill the new default-locale entries and their ICU arguments before building.");
    }
    Ok(())
}

fn catalog_path(project: &Path, locale: &Locale) -> PathBuf {
    project.join("locales").join(format!("{locale}.json"))
}

fn merge(
    project: &Path,
    discovery: &ProjectDiscovery,
    catalogs: &mut Catalogs,
    locale: &Locale,
    updates: SourceMessages,
) -> Result<()> {
    let config = discovery
        .config
        .as_ref()
        .context("locale configuration is missing")?;
    let defaults = &catalogs[&config.default];
    if locale != &config.default {
        for key in updates.keys() {
            if !defaults.contains_key(key) {
                bail!("message {key} is absent from the default catalog");
            }
        }
    }
    let count = updates.len();
    catalogs
        .get_mut(locale)
        .context("target locale is missing")?
        .extend(updates);
    let sources = catalogs
        .iter()
        .map(|(locale, messages)| {
            Ok(CatalogSource {
                locale: locale.clone(),
                file: discovery
                    .catalogs
                    .iter()
                    .find(|source| source.locale == *locale)
                    .map_or(0, |source| source.file),
                source: String::from_utf8(json(messages)?)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let compiled = pocopine_locale::server::compile_catalogs(
        &config.validate()?,
        &sources,
        &discovery.extracted.references,
    );
    if compiled.has_errors() {
        for diagnostic in compiled
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
        {
            eprintln!("error: {}", diagnostic.message);
        }
        bail!("translation update failed validation; catalog was not changed");
    }
    let path = catalog_path(project, locale);
    super::write(&path, &json(&catalogs[locale])?)?;
    println!("✓ merged {count} messages into {}", path.display());
    Ok(())
}

#[derive(Serialize)]
struct Coverage {
    locale: Locale,
    direct: usize,
    fallback: usize,
    orphaned: usize,
    total: usize,
}

fn stats(discovery: &ProjectDiscovery, catalogs: &Catalogs, as_json: bool) -> Result<()> {
    let compiled = check(discovery)?;
    let total = compiled.messages.len();
    let rows: Vec<_> = catalogs
        .iter()
        .map(|(locale, messages)| {
            let direct = messages
                .keys()
                .filter(|key| compiled.messages.contains_key(*key))
                .count();
            Coverage {
                locale: locale.clone(),
                direct,
                fallback: total - direct,
                orphaned: messages.len() - direct,
                total,
            }
        })
        .collect();
    if as_json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        let mut table = comfy_table::Table::new();
        table.set_header(["Locale", "Direct", "Fallback", "Orphaned"]);
        for row in rows {
            table.add_row([
                row.locale.to_string(),
                format!("{}/{}", row.direct, row.total),
                row.fallback.to_string(),
                row.orphaned.to_string(),
            ]);
        }
        println!("{table}");
    }
    Ok(())
}
