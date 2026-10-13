//! Checks registered feature rows in the configuration reference shipped by the pager.

use xai_grok_shell::agent::config::FEATURES;

const CONFIG_REFERENCE: &str = include_str!("../docs/user-guide/26-config-reference.md");

#[derive(Debug, PartialEq, Eq)]
enum RowError {
    RowCount(usize),
    Malformed,
    Environment,
    Requirements,
    Managed,
}

fn validate_feature_row(markdown: &str, path: &str, env: &str) -> Result<(), RowError> {
    let key = format!("`{path}`");
    let rows: Vec<Vec<&str>> = markdown
        .lines()
        .filter_map(|line| {
            let content = line.trim().strip_prefix('|')?.strip_suffix('|')?;
            let cells: Vec<&str> = content.split('|').map(str::trim).collect();
            (cells.first().copied() == Some(key.as_str())).then_some(cells)
        })
        .collect();
    let [row] = rows.as_slice() else {
        return Err(RowError::RowCount(rows.len()));
    };
    if row.len() != 5 {
        return Err(RowError::Malformed);
    }
    if !row[4]
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|token| token == env)
    {
        return Err(RowError::Environment);
    }
    if !matches!(row[2], "pin" | "`pin`") {
        return Err(RowError::Requirements);
    }
    if !matches!(row[3], "user" | "`user`") {
        return Err(RowError::Managed);
    }
    Ok(())
}

#[test]
fn every_registered_feature_reaches_the_operator() {
    for spec in FEATURES {
        let result = validate_feature_row(CONFIG_REFERENCE, spec.path, spec.env);
        assert!(
            result.is_ok(),
            "{} has an invalid feature row in 26-config-reference.md: {result:?}",
            spec.path,
        );
    }
}

const EXAMPLE_ROW: &str =
    "| `features.example` | `boolean` | `pin` | `user` | Also `GROK_EXAMPLE`. |";

#[test]
fn exact_row_and_environment_token_are_accepted() {
    assert_eq!(
        validate_feature_row(EXAMPLE_ROW, "features.example", "GROK_EXAMPLE"),
        Ok(()),
    );
}

#[test]
fn missing_row_is_not_satisfied_by_prose() {
    assert_eq!(
        validate_feature_row(
            "Use `features.example` and `GROK_EXAMPLE`.",
            "features.example",
            "GROK_EXAMPLE",
        ),
        Err(RowError::RowCount(0)),
    );
}

#[test]
fn duplicate_rows_are_rejected() {
    let markdown = format!("{EXAMPLE_ROW}\n{EXAMPLE_ROW}");
    assert_eq!(
        validate_feature_row(&markdown, "features.example", "GROK_EXAMPLE"),
        Err(RowError::RowCount(2)),
    );
}

#[test]
fn path_in_another_cell_does_not_match() {
    let markdown = EXAMPLE_ROW.replace("`boolean`", "`features.other`");
    assert_eq!(
        validate_feature_row(&markdown, "features.other", "GROK_EXAMPLE"),
        Err(RowError::RowCount(0)),
    );
}

#[test]
fn neighboring_feature_name_does_not_match() {
    let markdown = EXAMPLE_ROW.replace("features.example", "features.example_extra");
    assert_eq!(
        validate_feature_row(&markdown, "features.example", "GROK_EXAMPLE"),
        Err(RowError::RowCount(0)),
    );
}

#[test]
fn environment_from_another_row_does_not_match() {
    let markdown = format!(
        "{}\n| `features.other` | `boolean` | `pin` | `user` | Also `GROK_EXAMPLE`. |",
        EXAMPLE_ROW.replace("GROK_EXAMPLE", "GROK_OTHER"),
    );
    assert_eq!(
        validate_feature_row(&markdown, "features.example", "GROK_EXAMPLE"),
        Err(RowError::Environment),
    );
}

#[test]
fn environment_identifier_prefixes_do_not_match() {
    for identifier in ["GROK_EXAMPLE_EXTRA", "OTHER_GROK_EXAMPLE"] {
        let markdown = EXAMPLE_ROW.replace("GROK_EXAMPLE", identifier);
        assert_eq!(
            validate_feature_row(&markdown, "features.example", "GROK_EXAMPLE"),
            Err(RowError::Environment),
        );
    }
}

#[test]
fn wrong_requirements_cell_is_rejected() {
    let markdown = EXAMPLE_ROW.replace("`pin`", "`yes`");
    assert_eq!(
        validate_feature_row(&markdown, "features.example", "GROK_EXAMPLE"),
        Err(RowError::Requirements),
    );
}

#[test]
fn wrong_managed_cell_is_rejected() {
    let markdown = EXAMPLE_ROW.replace("`user`", "`fleet`");
    assert_eq!(
        validate_feature_row(&markdown, "features.example", "GROK_EXAMPLE"),
        Err(RowError::Managed),
    );
}

#[test]
fn malformed_row_is_rejected() {
    let markdown = EXAMPLE_ROW.replace(" | `boolean`", "");
    assert_eq!(
        validate_feature_row(&markdown, "features.example", "GROK_EXAMPLE"),
        Err(RowError::Malformed),
    );
}
