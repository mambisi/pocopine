use jiff::{
    Span, Timestamp, Zoned,
    civil::{Date, DateTime, Time},
};

use super::{TS, impl_primitives};

impl_primitives!(Date, DateTime, Span, Time, Timestamp, Zoned => "string");
