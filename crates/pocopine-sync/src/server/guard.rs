use std::future::Future;

use pocopine_core::ServerResult;
use pocopine_server::RequestContext;
use pocopine_server::auth::{Predicate, RequestAuthExt};

use super::*;

/// Server-side access check for a sync stream.
pub trait SyncStreamGuard: Send + Sync + 'static {
    /// Authorize one request before the stream source runs.
    fn check(&self, ctx: RequestContext) -> SyncGuardFuture<'_>;
}

impl<F, Fut> SyncStreamGuard for F
where
    F: Fn(RequestContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ServerResult<()>> + Send + 'static,
{
    fn check(&self, ctx: RequestContext) -> SyncGuardFuture<'_> {
        Box::pin((self)(ctx))
    }
}

pub(crate) struct PredicateStreamGuard<P>(pub(crate) P);

impl<P> SyncStreamGuard for PredicateStreamGuard<P>
where
    P: Predicate,
{
    fn check(&self, ctx: RequestContext) -> SyncGuardFuture<'_> {
        let result: ServerResult<()> = self.0.check(&ctx.principal()).into();
        Box::pin(async move { result })
    }
}
