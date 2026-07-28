#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::assertions_on_constants // cfg!(feature) asserts are intentional feature-gate checks
)]

// @ac AC-1420-04
#[test]
fn feature_gated_grammars_compile_independently() {
    // Verify: Each grammar is a separate Cargo feature
    //         (lang-rust, lang-typescript, lang-javascript, lang-python, lang-go).
    // Verify: Default features are empty — grammars are opt-in.
    // When features are enabled, the corresponding compress::code path works.

    // With default features (empty), no language grammars should be active.
    // Tests run with `cargo test -p ctxl` (default features = []).

    // Each grammar feature gates the corresponding compress::code path.
    #[cfg(feature = "lang-rust")]
    {
        let rust_out = ctxl::compress::code::compress("fn hello() { println!(\"hi\"); }", "rust");
        assert!(!rust_out.is_empty(), "lang-rust compress must return output");
    }

    #[cfg(feature = "lang-typescript")]
    {
        let ts_out = ctxl::compress::code::compress(
            "function greet(name: string): void { console.log(name); }",
            "typescript",
        );
        assert!(!ts_out.is_empty(), "lang-typescript compress must return output");
    }

    #[cfg(feature = "lang-javascript")]
    {
        let js_out =
            ctxl::compress::code::compress("function add(a, b) { return a + b; }", "javascript");
        assert!(!js_out.is_empty(), "lang-javascript compress must return output");
    }

    #[cfg(feature = "lang-python")]
    {
        let py_out =
            ctxl::compress::code::compress("def greet(name):\n    print(name)\n", "python");
        assert!(!py_out.is_empty(), "lang-python compress must return output");
    }

    #[cfg(feature = "lang-go")]
    {
        let go_out =
            ctxl::compress::code::compress("func hello() {\n\tfmt.Println(\"hi\")\n}\n", "go");
        assert!(!go_out.is_empty(), "lang-go compress must return output");
    }
}

// @ac AC-1420-07
#[test]
fn all_languages_feature_compiles_12_grammars() {
    // Verify: The `all-languages` feature in Cargo.toml activates all 12 grammar crates
    //         (Rust, TypeScript, JavaScript, Python, Go, Java, C, C++, Ruby, Bash, CSS, JSON).
    // Verify: Default feature set is empty (grammars are opt-in).

    // Count which of the 5 core languages are active.
    let core_active: usize = [
        cfg!(feature = "lang-rust"),
        cfg!(feature = "lang-typescript"),
        cfg!(feature = "lang-javascript"),
        cfg!(feature = "lang-python"),
        cfg!(feature = "lang-go"),
    ]
    .iter()
    .filter(|&&b| b)
    .count();

    // With default features = [], none of these should be active.
    #[cfg(not(feature = "all-languages"))]
    {
        assert_eq!(core_active, 0, "default feature set must be empty (no language grammars)");

        assert!(!cfg!(feature = "lang-java"), "lang-java must NOT be in the default feature set");
        assert!(!cfg!(feature = "lang-c"), "lang-c must NOT be in the default feature set");
        assert!(!cfg!(feature = "lang-cpp"), "lang-cpp must NOT be in the default feature set");
        assert!(!cfg!(feature = "lang-ruby"), "lang-ruby must NOT be in the default feature set");
        assert!(!cfg!(feature = "lang-bash"), "lang-bash must NOT be in the default feature set");
        assert!(!cfg!(feature = "lang-css"), "lang-css must NOT be in the default feature set");
        assert!(!cfg!(feature = "lang-json"), "lang-json must NOT be in the default feature set");
    }

    // When all-languages IS compiled in, all 12 should be present.
    #[cfg(feature = "all-languages")]
    {
        let all_active: usize = [
            cfg!(feature = "lang-rust"),
            cfg!(feature = "lang-typescript"),
            cfg!(feature = "lang-javascript"),
            cfg!(feature = "lang-python"),
            cfg!(feature = "lang-go"),
            cfg!(feature = "lang-java"),
            cfg!(feature = "lang-c"),
            cfg!(feature = "lang-cpp"),
            cfg!(feature = "lang-ruby"),
            cfg!(feature = "lang-bash"),
            cfg!(feature = "lang-css"),
            cfg!(feature = "lang-json"),
        ]
        .iter()
        .filter(|&&b| b)
        .count();

        assert_eq!(all_active, 12, "all-languages feature must activate exactly 12 grammars");
    }
}
