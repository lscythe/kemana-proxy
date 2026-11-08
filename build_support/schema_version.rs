const USER_VERSION_PRAGMA: &str = "PRAGMA USER_VERSION";

pub fn parse_schema_version(schema_sql: &str) -> Result<u32, String> {
    let mut pragmas = schema_sql
        .lines()
        .map(str::trim)
        .filter(|line| line.to_ascii_uppercase().starts_with(USER_VERSION_PRAGMA));
    let pragma = pragmas
        .next()
        .ok_or("the schema has no `PRAGMA user_version = N;` line")?;
    if pragmas.next().is_some() {
        return Err("the schema has more than one `PRAGMA user_version` line".to_owned());
    }
    version_from_pragma(pragma).ok_or_else(|| {
        format!("malformed schema version line {pragma:?}; expected `PRAGMA user_version = N;`")
    })
}

fn version_from_pragma(line: &str) -> Option<u32> {
    line.get(USER_VERSION_PRAGMA.len()..)?
        .trim_start()
        .strip_prefix('=')?
        .strip_suffix(';')?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_comes_from_the_user_version_pragma() {
        let schema = "-- `PRAGMA user_version` is the version.\n\
                      \n\
                      PRAGMA user_version = 7;\n\
                      CREATE TABLE stops (id TEXT);\n";

        assert_eq!(parse_schema_version(schema), Ok(7));
    }

    #[test]
    fn surrounding_spacing_and_keyword_case_do_not_matter() {
        assert_eq!(parse_schema_version("  pragma USER_VERSION=3;  \n"), Ok(3));
    }

    #[test]
    fn a_schema_without_the_pragma_is_reported_as_missing() {
        let error =
            parse_schema_version("-- PRAGMA user_version = 2;\nCREATE TABLE t (a);\n").unwrap_err();

        assert!(
            error.contains("no `PRAGMA user_version = N;` line"),
            "{error}"
        );
    }

    #[test]
    fn a_malformed_pragma_quotes_the_offending_line() {
        for line in [
            "PRAGMA user_version;",
            "PRAGMA user_version = two;",
            "PRAGMA user_version = 2",
            "PRAGMA user_version = -2;",
            "PRAGMA user_version = 2; -- current",
        ] {
            let error = parse_schema_version(line).unwrap_err();

            assert!(error.contains("malformed"), "{error}");
            assert!(error.contains(line), "{error}");
        }
    }

    #[test]
    fn more_than_one_pragma_is_ambiguous() {
        let schema = "PRAGMA user_version = 1;\nPRAGMA user_version = 2;\n";

        let error = parse_schema_version(schema).unwrap_err();

        assert!(error.contains("more than one"), "{error}");
    }
}
