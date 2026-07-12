// Duplicate #[job] keys used to last-one-win silently — the job ran
// on the wrong queue with no diagnostic.
use pocopine::JobResult;

#[pocopine::job(queue = "emails", queue = "reports")]
async fn send(recipient: String) -> JobResult<()> {
    let _ = recipient;
    Ok(())
}

fn main() {}
