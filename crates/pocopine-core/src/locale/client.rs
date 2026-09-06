//! Reactive browser locale selection over the generated API's shared cache.
//!
//! Boot installs the initial catalog and calls [`LocaleController::new`] with
//! `t::catalogs()?`. Activate that controller before mounting the application.
//! Fetching a later catalog never exposes a language without its catalog.

use std::{
    cell::RefCell,
    future::Future,
    rc::{Rc, Weak},
};

pub use pocopine_locale::client::ClientCatalogs;
use pocopine_locale::{Locale, Locales, MessageId, RenderedPart, TranslationError, Value};

use crate::{ServerError, Setter, Signal, signal};

mod boot;
pub use boot::boot;

thread_local! {
    static ACTIVE: RefCell<Option<LocaleController>> = const { RefCell::new(None) };
    static ACTIVATED: (Signal<bool>, Setter<bool>) = signal(false);
}

/// One browser application's committed locale. Cloning shares selection and
/// catalogs. The read-only signal can never be set ahead of catalog readiness.
#[derive(Clone)]
pub struct LocaleController(Rc<State>);

struct State {
    catalogs: ClientCatalogs,
    selected: Signal<Locale>,
    set_selected: Setter<Locale>,
    pending: RefCell<Option<Weak<()>>>,
    delivery: Option<pocopine_locale::LocaleManifest>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SwitchOutcome {
    Committed,
    Superseded,
}

impl LocaleController {
    /// `initial` must be an exact configured locale with a validated catalog.
    /// Negotiation belongs to boot/the router, before constructing this state.
    pub fn new(catalogs: ClientCatalogs, initial: Locale) -> Result<Self, TranslationError> {
        Self::with_delivery(catalogs, initial, None)
    }

    fn with_delivery(
        catalogs: ClientCatalogs,
        initial: Locale,
        delivery: Option<pocopine_locale::LocaleManifest>,
    ) -> Result<Self, TranslationError> {
        require_supported(catalogs.locales(), &initial)?;
        catalogs.catalog(&initial)?;
        let (selected, set_selected) = signal(initial);
        Ok(Self(Rc::new(State {
            catalogs,
            selected,
            set_selected,
            pending: RefCell::new(None),
            delivery,
        })))
    }

    /// Bind the controller to this browser boot. Repeating activation of the
    /// same controller is harmless; replacing it would strand mounted effects
    /// and is rejected. No corresponding ambient host state exists.
    pub fn activate(&self) -> Result<(), TranslationError> {
        let newly_active = ACTIVE.with(|slot| {
            let mut slot = slot.borrow_mut();
            if let Some(existing) = slot.as_ref() {
                if Rc::ptr_eq(&existing.0, &self.0) {
                    return Ok(false);
                }
                return Err(TranslationError::Initialization(
                    "a different browser locale controller is already active".into(),
                ));
            }
            *slot = Some(self.clone());
            Ok(true)
        })?;
        // Publish only after releasing ACTIVE's borrow. Bindings mounted too
        // early can leave their diagnostic state once boot has validated data.
        if newly_active {
            ACTIVATED.with(|(_, setter)| setter.set(true));
        }
        Ok(())
    }

    pub fn locales(&self) -> &Locales {
        self.0.catalogs.locales()
    }

    pub fn build_id(&self) -> &str {
        self.0.catalogs.build_id()
    }

    /// Reactive read for computed text and template effects.
    pub fn locale(&self) -> Locale {
        self.0.selected.get()
    }

    /// Fixed boundary value, e.g. an RPC or semantic recipient-message input.
    /// This read does not make an enclosing effect depend on language changes.
    pub fn snapshot(&self) -> Locale {
        self.0.selected.get_untracked()
    }

    pub fn signal(&self) -> Signal<Locale> {
        self.0.selected.clone()
    }

    fn update_document(&self, locale: &Locale) -> Result<(), TranslationError> {
        if let Some(manifest) = &self.0.delivery {
            boot::update_document(locale, manifest.directions[locale])?;
        }
        Ok(())
    }

    pub fn format(
        &self,
        id: MessageId,
        args: &[(&str, Value<'_>)],
    ) -> Result<String, TranslationError> {
        self.0.catalogs.format(&self.locale(), id, args)
    }

    pub fn render(
        &self,
        id: MessageId,
        args: &[(&str, Value<'_>)],
    ) -> Result<Vec<RenderedPart>, TranslationError> {
        self.0.catalogs.catalog(&self.locale())?.render(id, args)
    }

    /// Display server-owned public payloads verbatim. Network diagnostics stay
    /// on the error; only their public text uses the application's generated
    /// arg-less message function and follows the current UI language.
    pub fn error_message(&self, error: &ServerError, network: fn(Locale) -> String) -> String {
        match error.public_message() {
            Some(message) => message.to_owned(),
            None => network(self.locale()),
        }
    }

    /// Start a selection immediately, before any asynchronous work. A newer
    /// selection supersedes this ticket, even when it selects the current
    /// language. Dropping a ticket cancels its ability to commit.
    pub fn begin_switch(&self, target: Locale) -> Result<LocaleSwitch, TranslationError> {
        require_supported(self.locales(), &target)?;
        let token = Rc::new(());
        *self.0.pending.borrow_mut() = Some(Rc::downgrade(&token));
        Ok(LocaleSwitch {
            controller: self.clone(),
            target,
            token,
        })
    }

    /// Load a language from this boot's fingerprinted catalog map. A failed
    /// request leaves the current language intact; calling again retries it.
    pub async fn set_locale(&self, target: Locale) -> Result<SwitchOutcome, TranslationError> {
        let manifest = self.0.delivery.as_ref().ok_or_else(|| {
            TranslationError::Initialization("controller has no catalog delivery manifest".into())
        })?;
        let url = manifest.catalogs.get(&target).ok_or_else(|| {
            TranslationError::Initialization(format!("unsupported UI locale {target}"))
        })?;
        self.switch_with(target, |_| boot::load_catalog(url)).await
    }

    /// Fetch on a cache miss and atomically commit only the newest selection.
    /// Cancelling/dropping this future leaves the committed locale intact.
    /// Superseded responses, including failures, do not alter the cache.
    pub async fn switch_with<F, Fut>(
        &self,
        target: Locale,
        load: F,
    ) -> Result<SwitchOutcome, TranslationError>
    where
        F: FnOnce(Locale) -> Fut,
        Fut: Future<Output = Result<Vec<u8>, TranslationError>>,
    {
        let ticket = self.begin_switch(target)?;
        if !ticket.needs_catalog() {
            return ticket.commit(None);
        }
        let bytes = load(ticket.target().clone()).await;
        if !ticket.is_current() {
            return Ok(SwitchOutcome::Superseded);
        }
        ticket.commit(Some(&bytes?))
    }
}

/// A cancellation-safe pending selection. Catalog loading is deliberately
/// outside this ticket, so the HTML loader/router can supply its own transport.
#[must_use = "dropping the ticket cancels this selection"]
pub struct LocaleSwitch {
    controller: LocaleController,
    target: Locale,
    token: Rc<()>,
}

impl LocaleSwitch {
    pub fn target(&self) -> &Locale {
        &self.target
    }

    pub fn needs_catalog(&self) -> bool {
        self.controller.0.catalogs.catalog(&self.target).is_err()
    }

    pub fn is_current(&self) -> bool {
        self.controller
            .0
            .pending
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending.ptr_eq(&Rc::downgrade(&self.token)))
    }

    /// Validate optional fetched bytes, then publish the locale. With `None`,
    /// the catalog must already be cached. Validation failures publish nothing.
    /// No callback or await occurs between validation and selection update.
    pub fn commit(self, bytes: Option<&[u8]>) -> Result<SwitchOutcome, TranslationError> {
        if !self.is_current() {
            return Ok(SwitchOutcome::Superseded);
        }
        if let Some(bytes) = bytes {
            self.controller
                .0
                .catalogs
                .install(self.target.clone(), bytes)?;
        }
        self.controller.0.catalogs.catalog(&self.target)?;
        self.controller.update_document(&self.target)?;
        // Release borrows and retire this ticket before notifying effects:
        // custom synchronous schedulers may begin another switch reentrantly.
        self.controller.0.pending.borrow_mut().take();
        self.controller.0.set_selected.set(self.target.clone());
        Ok(SwitchOutcome::Committed)
    }
}

impl Drop for LocaleSwitch {
    fn drop(&mut self) {
        if self.is_current() {
            self.controller.0.pending.borrow_mut().take();
        }
    }
}

fn require_supported(locales: &Locales, locale: &Locale) -> Result<(), TranslationError> {
    if locales.supported().any(|supported| supported == locale) {
        Ok(())
    } else {
        Err(TranslationError::Initialization(format!(
            "unsupported UI locale {locale}"
        )))
    }
}

/// Read the active browser controller. Used by compiled template bindings.
pub fn active() -> Result<LocaleController, TranslationError> {
    let controller = ACTIVE.with(|slot| slot.borrow().clone());
    if controller.is_none() {
        ACTIVATED.with(|(signal, _)| signal.get());
    }
    controller.ok_or(TranslationError::NotInitialized)
}

pub(crate) fn rpc_locale() -> Option<Locale> {
    ACTIVE.with(|slot| slot.borrow().as_ref().map(LocaleController::snapshot))
}
