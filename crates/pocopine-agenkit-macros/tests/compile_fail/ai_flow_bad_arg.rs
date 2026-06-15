#[pocopine_agenkit_macros::ai_flow(nope)]
async fn f(input: u8, ctx: u8) -> Result<u8, u8> {
    Ok(input)
}

fn main() {}
