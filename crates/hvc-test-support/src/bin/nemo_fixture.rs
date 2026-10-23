//! A stand-in for NeMo-Speech.cpp (ADR-029): answers `--version`,
//! `--json doctor` and `transcribe` the way the pinned runtime does, so the
//! voice runtime can be installed and driven without the real model.
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["--version"] => {
            println!("nemo-speech 0.2.0");
            ExitCode::SUCCESS
        }
        ["--json", "doctor"] => {
            println!(r#"{{"devices":[{{"name":"CPU","type":"cpu"}}],"features":{{"asr":true}}}}"#);
            ExitCode::SUCCESS
        }
        [
            "--json",
            "--quiet",
            "transcribe",
            audio,
            "--model",
            model,
            "--device",
            "cpu",
            "-f",
            "json",
        ] => {
            if !std::path::Path::new(model).is_file() {
                eprintln!(
                    r#"{{"error":{{"message":"model not found","type":"invalid_argument","command":"transcribe","exit_code":2}}}}"#
                );
                return ExitCode::from(2);
            }
            let Ok(wav) = std::fs::read(audio) else {
                eprintln!(
                    r#"{{"error":{{"message":"{audio} is not a file or directory","type":"invalid_argument","command":"transcribe","exit_code":2}}}}"#
                );
                return ExitCode::from(2);
            };
            // Silence transcribes to nothing, anything else to a fixed sentence.
            let heard = wav.len() > 44 && wav[44..].iter().any(|byte| *byte != 0);
            let text = if heard {
                "Fixture transcript of the note."
            } else {
                ""
            };
            println!(
                r#"{{"file":"{}","text":"{text}","confidence":1,"duration":1.0,"languages":["pt-BR"],"words":[]}}"#,
                audio.replace('\\', "\\\\")
            );
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                r#"{{"error":{{"message":"unexpected arguments: {}","type":"invalid_argument","command":"","exit_code":2}}}}"#,
                args.join(" ")
            );
            ExitCode::from(2)
        }
    }
}
