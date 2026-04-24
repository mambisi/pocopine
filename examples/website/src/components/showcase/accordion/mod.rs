use pine::{PineAccordionContent, PineAccordionItem, PineAccordionRoot, PineAccordionTrigger};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "AccordionDemo.poco",
    style = "accordion.css",
    role = "panel",
    // RFC 049 — opt into slot-contract validation. Accordion
    // enforces structure at two levels:
    //   Root → [Item]
    //   Item → [Trigger, Content]
    // Misplaced items (e.g. a bare Trigger at the Root, or a
    // Content outside an Item) are caught at compile time.
    uses = [
        PineAccordionRoot,
        PineAccordionItem,
        PineAccordionTrigger,
        PineAccordionContent,
    ]
)]
pub struct AccordionDemo {
    pub value: String,
}

#[handlers]
impl AccordionDemo {}
