//! `agenkitty-skills` — binary execution mode for the skill loader (RFC-121).

fn main() {
    std::process::exit(agenkitty_skills::cli::run());
}
