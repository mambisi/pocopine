use super::{FrameworkEvent, FrameworkEventKind};

pub fn render_human(events: &[FrameworkEvent]) {
    for event in events {
        match &event.kind {
            FrameworkEventKind::Started => {
                if let Some(message) = &event.message {
                    println!("{message}");
                }
            }
            FrameworkEventKind::AssistantText => {
                if let Some(message) = &event.message {
                    println!("{message}");
                }
            }
            FrameworkEventKind::ToolStarted => {
                println!(
                    "tool started: {}",
                    event.tool.as_deref().unwrap_or("<unknown>")
                );
            }
            FrameworkEventKind::ToolCompleted => {
                println!(
                    "tool completed: {}",
                    event.tool.as_deref().unwrap_or("<unknown>")
                );
            }
            FrameworkEventKind::ToolFailed | FrameworkEventKind::ToolBlocked => {
                println!(
                    "tool issue: {} {}",
                    event.tool.as_deref().unwrap_or("<unknown>"),
                    event.message.as_deref().unwrap_or("")
                );
            }
            FrameworkEventKind::Compacted => println!("session compacted"),
            FrameworkEventKind::Stopped { status } => println!("stopped: {status:?}"),
            FrameworkEventKind::Failed => {
                println!("failed: {}", event.message.as_deref().unwrap_or("unknown"))
            }
            FrameworkEventKind::Unknown => {
                println!("{}", event.message.as_deref().unwrap_or("unknown event"))
            }
        }
    }
}
