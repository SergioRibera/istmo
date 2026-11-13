//! Message shapes beyond plain named-field structs.

use istmo::message;

/// Enum with unit, single and multi-field variants.
#[message]
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// No payload.
    Dot,
    /// Radius.
    Circle(f64),
    /// Points and a label.
    Path(Vec<i32>, Option<String>),
}

/// Generic wrapper.
#[message]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrapper<T> {
    /// Wrapped value.
    pub inner: T,
}

/// Tuple struct.
#[message]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pair(pub u32, pub u32);

/// Unit struct.
#[message]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nothing;

/// Field-less enum.
#[message]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// In.
    In,
    /// Out.
    Out,
}
