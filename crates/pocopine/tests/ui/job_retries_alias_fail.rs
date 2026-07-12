// `retries` and `max_retries` are aliases for the same knob — passing
// both silently kept whichever came later.
use pocopine::JobResult;

#[pocopine::job(queue = "emails", retries = 3, max_retries = 9)]
async fn send(recipient: String) -> JobResult<()> {
    let _ = recipient;
    Ok(())
}

fn main() {}
