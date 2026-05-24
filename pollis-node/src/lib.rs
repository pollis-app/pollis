#[macro_use]
extern crate napi_derive;

#[napi]
pub fn ping() -> String {
    "pong from pollis-core".to_string()
}
