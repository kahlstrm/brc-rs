use std::{fs, path::Path, process::Command};

#[test]
fn prints_exactly_the_expected_output_for_every_sample() {
    let samples = fs::read_dir("samples")
        .unwrap()
        .map(|entry| entry.unwrap().path());
    for input in samples.filter(|path| path.extension().is_some_and(|ext| ext == "txt")) {
        let output = Command::new(env!("CARGO_BIN_EXE_brc-rs"))
            .arg(&input)
            .output()
            .unwrap();
        assert!(output.status.success(), "{input:?}");
        let expected = fs::read_to_string(Path::new(&input).with_extension("out")).unwrap();
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            expected,
            "{input:?}"
        );
    }
}
