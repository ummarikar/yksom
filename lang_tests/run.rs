// Copyright (c) 2019 King's College London created by the Software Development Team
// <http://soft-dev.org/>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0>, or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, or the UPL-1.0 license <http://opensource.org/licenses/UPL>
// at your option. This file may not be copied, modified, or distributed except according to those
// terms.

use std::process::Command;

use lang_tester::LangTester;
use lazy_static::lazy_static;
use regex::{Regex, RegexBuilder};

const SOM_LIBS_PATH: &'static str = "lib/SOM/";

lazy_static! {
    static ref EXPECTED: Regex = RegexBuilder::new(r#"^"(.*?)^"[ \t]*$"#)
        .multi_line(true)
        .dot_matches_new_line(true)
        .build()
        .unwrap();
}

fn main() {
    LangTester::new()
        .test_dir("lang_tests")
        .test_file_filter(|p| p.extension().unwrap().to_str().unwrap() == "som")
        .test_extract(|s| {
            EXPECTED
                .captures(s)
                .map(|x| x.get(1).unwrap().as_str().trim().to_owned())
        })
        .test_cmds(|p| {
            let mut vm = Command::new("cargo");
            vm.args(&[
                "run",
                "-q",
                #[cfg(not(debug_assertions))]
                "--release",
                "--",
                "--cp",
                SOM_LIBS_PATH,
                p.to_str().unwrap(),
            ]);
            vec![("VM", vm)]
        })
        .run();
}
