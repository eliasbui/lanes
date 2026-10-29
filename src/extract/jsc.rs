//! The challenges in the addresses YouTube gives a signed-in viewer: a signature (`s`) and an `n` value that functions
//! inside the player's script (base.js) transform, functions that change with every version of the player. The
//! player's own address builder calls `set("alr", "yes")`; the solver of yt-dlp's EJS project finds that builder in
//! the script's syntax tree and runs it on each challenge (https://github.com/yt-dlp/ejs, Unlicense, carrying meriyah
//! under ISC and astring under MIT, whose notices head ejs/lib.min.js). It runs here in an embedded QuickJS, so nothing
//! has to be installed. The player it prepares is kept in the data folder by the player's version, which makes the
//! next answers fast.

use serde_json::{Value, json};
use std::{collections::HashMap, fs, path::PathBuf};

const LIB: &str = include_str!("ejs/lib.min.js");
const CORE: &str = include_str!("ejs/core.min.js");
/// The parse of a 2.6 MB player goes deep: QuickJS gets a stack to match, on a thread of its own.
const STACK: usize = 256 * 1024 * 1024;
const MEMORY: usize = 2 * 1024 * 1024 * 1024;

/// What each challenge becomes, by kind ("n" or "sig") and then by challenge.
pub type Answers = HashMap<String, HashMap<String, String>>;

fn cache(player: &str) -> PathBuf {
    crate::data_dir().join("youtube").join(format!("{player}.js"))
}

/// Answers the challenges for the player `player` (its version id). `script` fetches the player when it has not been
/// prepared before.
pub fn solve(player: &str, script: impl FnOnce() -> Result<String, String>, requests: &[(&str, Vec<String>)]) -> Result<Answers, String> {
    let requests: Vec<Value> = requests.iter().filter(|(_, c)| !c.is_empty()).map(|(kind, challenges)| json!({ "type": kind, "challenges": challenges })).collect();
    if requests.is_empty() {
        return Ok(Answers::new());
    }
    let prepared = fs::read_to_string(cache(player)).ok();
    let input = match &prepared {
        Some(prepared) => json!({ "type": "preprocessed", "preprocessed_player": prepared, "requests": requests }),
        None => json!({ "type": "player", "player": script()?, "requests": requests, "output_preprocessed": true }),
    };
    let output = std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(move || run(&input.to_string()))
        .map_err(|e| e.to_string())?
        .join()
        .map_err(|_| "Bộ giải của YouTube dừng bất thường".to_owned())??;
    if let Some(prepared) = output["preprocessed_player"].as_str() {
        let path = cache(player);
        // Only the latest player is kept: an older one is not asked for again.
        if let Some(folder) = path.parent() {
            let _ = fs::remove_dir_all(folder);
            let _ = fs::create_dir_all(folder);
        }
        let _ = fs::write(&path, prepared);
    }
    answers(&output)
}

/// The solver's answer, by kind and challenge; one kind it could not answer fails the whole.
fn answers(output: &Value) -> Result<Answers, String> {
    if output["type"] != "result" {
        return Err(format!("Bộ giải của YouTube báo lỗi: {}", output["error"].as_str().unwrap_or("không rõ")));
    }
    let mut answers = Answers::new();
    let kinds = output["kinds"].as_array().into_iter().flatten().filter_map(Value::as_str);
    for (kind, response) in kinds.zip(output["responses"].as_array().into_iter().flatten()) {
        if response["type"] != "result" {
            return Err(format!("Không giải được mã của YouTube: {}", response["error"].as_str().unwrap_or("không rõ").lines().next().unwrap_or_default()));
        }
        let solved = answers.entry(kind.to_owned()).or_default();
        for (challenge, answer) in response["data"].as_object().into_iter().flatten() {
            if let Some(answer) = answer.as_str() {
                solved.insert(challenge.clone(), answer.to_owned());
            }
        }
    }
    Ok(answers)
}

/// The solver on `input` in a fresh QuickJS, its answer as JSON. The kinds asked are echoed beside the responses,
/// which come back in the same order.
fn run(input: &str) -> Result<Value, String> {
    let runtime = rquickjs::Runtime::new().map_err(|e| e.to_string())?;
    runtime.set_memory_limit(MEMORY);
    runtime.set_max_stack_size(STACK - 1024 * 1024);
    let context = rquickjs::Context::full(&runtime).map_err(|e| e.to_string())?;
    context.with(|ctx| {
        ctx.globals().set("input", input).map_err(|e| e.to_string())?;
        let script = format!(
            "{LIB}\nObject.assign(globalThis, lib);\n{CORE}\nconst request = JSON.parse(input);\nconst output = jsc(request);\noutput.kinds = request.requests.map((r) => r.type);\nJSON.stringify(output);"
        );
        let text: String = match ctx.eval(script) {
            Ok(text) => text,
            Err(rquickjs::Error::Exception) => {
                let thrown = ctx.catch();
                let message = thrown.as_exception().and_then(|e| e.message()).or_else(|| thrown.as_string().and_then(|s| s.to_string().ok())).unwrap_or_default();
                return Err(format!("Bộ giải của YouTube báo lỗi: {message}"));
            }
            Err(e) => return Err(e.to_string()),
        };
        serde_json::from_str(&text).map_err(|e| e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_read_by_kind_and_a_failed_kind_fails_them_all() {
        let output = json!({ "type": "result", "kinds": ["n", "sig"], "responses": [
            { "type": "result", "data": { "abc": "xyz" } },
            { "type": "result", "data": { "s1": "t1" } },
        ]});
        let solved = answers(&output).unwrap();
        assert_eq!(solved["n"]["abc"], "xyz");
        assert_eq!(solved["sig"]["s1"], "t1");
        let failed = json!({ "type": "result", "kinds": ["n"], "responses": [{ "type": "error", "error": "Failed to extract n function\nstack" }] });
        assert_eq!(answers(&failed).unwrap_err(), "Không giải được mã của YouTube: Failed to extract n function");
    }

    /// The solver in QuickJS on a real player: `LANES_PLAYER=<path to base.js> cargo test jsc -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn the_solver_runs_in_quickjs_on_a_real_player() {
        let player = fs::read_to_string(std::env::var("LANES_PLAYER").expect("LANES_PLAYER")).unwrap();
        let input = json!({ "type": "player", "player": player, "output_preprocessed": true, "requests": [
            { "type": "n", "challenges": ["ZdZIqFPQK-Ty8wId"] },
        ]});
        let started = std::time::Instant::now();
        let output = std::thread::Builder::new().stack_size(STACK).spawn(move || run(&input.to_string())).unwrap().join().unwrap().unwrap();
        println!("first: {:.1} s", started.elapsed().as_secs_f64());
        let solved = answers(&output).unwrap();
        println!("{solved:?}");
        assert!(solved["n"]["ZdZIqFPQK-Ty8wId"].len() > 5);
        let prepared = output["preprocessed_player"].as_str().unwrap().to_owned();
        let input = json!({ "type": "preprocessed", "preprocessed_player": prepared, "requests": [{ "type": "n", "challenges": ["ZdZIqFPQK-Ty8wId"] }] });
        let started = std::time::Instant::now();
        let again = std::thread::Builder::new().stack_size(STACK).spawn(move || run(&input.to_string())).unwrap().join().unwrap().unwrap();
        println!("prepared: {:.1} s", started.elapsed().as_secs_f64());
        assert_eq!(answers(&again).unwrap()["n"], solved["n"]);
    }
}
