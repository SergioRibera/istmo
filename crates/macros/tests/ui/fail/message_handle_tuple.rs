pub enum Credential {}

#[istmo::message]
pub struct Account(#[handle(Credential)] istmo::NativeHandleId);

fn main() {}
