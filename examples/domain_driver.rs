//! Test-only bridge for socket-free browser checks of the real domain reducer.
use serde_json::{json, Value};
use stateset_warehouse::domain::{demo_state, Command, State};
use std::io::{self, Read};
fn main() {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .expect("read test input");
    if input.trim().is_empty() {
        println!("{}", json!(demo_state()));
        return;
    }
    let v: Value = serde_json::from_str(&input).expect("test JSON");
    let s: State = serde_json::from_value(v["state"].clone()).expect("test state");
    if v["command"].is_null() {
        println!(
            "{}",
            json!({"ok":s.validate().is_ok(),"version":s.version,"rows":s.reconciliation()})
        );
        return;
    }
    let c: Command = serde_json::from_value(v["command"].clone()).expect("test command");
    match s.execute(&c, "operator:demo") {
        Ok((state, result)) => println!("{}", json!({"state":state,"result":result})),
        Err(error) => println!("{}", json!({"error":error})),
    }
}
