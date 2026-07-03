//! The host approval seam (M1d): every Ask gate — the central dispatch gate,
//! the secret-grant gate, and MCP admission — resolves through one
//! [`ToolApprover`]. No approver installed means every Ask fails closed, which
//! is the same headless semantics the gates shipped with.

use std::io::{BufRead, IsTerminal, Write};

use agenkitty_core::policy::{ApprovalDecision, ApprovalRequest};
use futures::future::BoxFuture;

use crate::tools::session::redact_json_value;

/// A host-side approver for Ask-gated tool calls. Implementations decide one
/// request at a time; the gates await the decision, so a slow human answer
/// simply holds the one call that asked.
pub trait ToolApprover: Send + Sync {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision>;
}

/// The reason used when no approver is installed (headless run): Ask fails
/// closed, and the model is told why in one consistent sentence.
pub fn no_approver_reason(reason: &str) -> String {
    format!("{reason}; no approver is configured")
}

/// Non-interactive auto-approver: approves every request. This is the explicit
/// opt-out from the fail-closed default — a host installs it (e.g. the CLI's
/// `--yes` flag) to run write/command/`Ask`-class tools in CI or a piped
/// invocation where no operator can answer a prompt. It must never be the
/// default; installing it means the caller has accepted running unattended.
pub struct AutoApprover;

impl ToolApprover for AutoApprover {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
        // The request still routes through here (and is auditable), it is just
        // answered "approved" without a human.
        tracing::info!(
            target: "pocopine.log",
            tool = %request.tool_id,
            "auto-approved (--yes)"
        );
        Box::pin(async { ApprovalDecision::Approved })
    }
}

/// Interactive terminal approver: prints the request (bounded + redacted) to
/// stderr and reads `y`/`N` from stdin on a blocking thread. Anything but an
/// explicit `y`/`yes` is a denial — including EOF and a non-terminal stdin, so
/// piping input cannot silently auto-approve.
pub struct TtyApprover;

impl TtyApprover {
    /// The approver for this process, if stdin+stderr are attached to a
    /// terminal — otherwise `None` (headless: Ask fails closed).
    pub fn if_interactive() -> Option<Self> {
        (std::io::stdin().is_terminal() && std::io::stderr().is_terminal()).then_some(Self)
    }
}

impl ToolApprover for TtyApprover {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
        Box::pin(async move {
            let outcome = tokio::task::spawn_blocking(move || prompt_on_tty(&request)).await;
            outcome.unwrap_or_else(|_| ApprovalDecision::denied("approval prompt failed"))
        })
    }
}

fn prompt_on_tty(request: &ApprovalRequest) -> ApprovalDecision {
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "agenkitty approval required: {}", request.reason);
    if let Some(detail) = &request.detail {
        // Bounded + redacted before it reaches the operator's terminal.
        let rendered = redact_json_value(detail, 512);
        let _ = writeln!(stderr, "  {}: {rendered}", request.tool_id);
    }
    let _ = write!(stderr, "  approve `{}`? [y/N] ", request.tool_id);
    let _ = stderr.flush();

    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return ApprovalDecision::denied("approval prompt failed");
    }
    let answer = line.trim();
    if answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes") {
        ApprovalDecision::Approved
    } else {
        ApprovalDecision::denied("denied by operator")
    }
}

/// A scripted approver for tests: approves iff the constructed flag says so.
#[cfg(test)]
pub(crate) struct StaticApprover(pub bool);

#[cfg(test)]
impl ToolApprover for StaticApprover {
    fn approve<'a>(&'a self, _request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
        let approve = self.0;
        Box::pin(async move {
            if approve {
                ApprovalDecision::Approved
            } else {
                ApprovalDecision::denied("denied by operator")
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn static_approver_round_trips() {
        let approver = StaticApprover(true);
        let decision = approver
            .approve(ApprovalRequest::new("fs.write", "write-class tool"))
            .await;
        assert!(decision.is_approved());
    }

    #[tokio::test]
    async fn auto_approver_approves_every_request() {
        // The `--yes` escape hatch: approves without a human so headless/CI
        // runs can execute Ask-gated tools.
        let decision = AutoApprover
            .approve(ApprovalRequest::new("fs.write", "headless run"))
            .await;
        assert!(decision.is_approved());
    }
}
