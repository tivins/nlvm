use anyhow::{bail, Context, Result};

use crate::header::Header;

pub struct SourceBlock {
    pub path: String,
    pub content: String,
}

pub struct TestFile {
    pub header: Header,
    pub blocks: Vec<SourceBlock>,
}

pub fn parse_test_file(content: &str) -> Result<TestFile> {
    let mut lines = content.lines();

    let first = lines.next().context("empty test file")?;
    if first.trim() != "---" {
        bail!("test file must start with '---' front matter delimiter");
    }

    let mut yaml_lines = Vec::new();
    let mut found_closing = false;
    for line in lines.by_ref() {
        if line.trim() == "---" {
            found_closing = true;
            break;
        }
        yaml_lines.push(line);
    }
    if !found_closing {
        bail!("missing closing '---' for front matter");
    }

    let yaml_str = yaml_lines.join("\n");
    let header: Header = if yaml_str.trim().is_empty() {
        Header::default()
    } else {
        serde_yaml::from_str(&yaml_str).context("parsing YAML front matter")?
    };

    let separator = header.file_separator_or_default().to_string();
    let body_lines: Vec<&str> = lines.collect();
    let blocks = parse_blocks(&body_lines, &separator);

    Ok(TestFile { header, blocks })
}

fn parse_blocks(body_lines: &[&str], separator: &str) -> Vec<SourceBlock> {
    let mut blocks = Vec::new();
    let mut current_path: Option<String> = None;
    let mut current_content = String::new();

    for line in body_lines {
        if let Some(rest) = line.strip_prefix(separator) {
            if let Some(path) = current_path.take() {
                blocks.push(SourceBlock {
                    path,
                    content: std::mem::take(&mut current_content),
                });
            }
            current_path = Some(rest.trim().to_string());
        } else if current_path.is_some() {
            current_content.push_str(line);
            current_content.push('\n');
        }
        // Lines before the first separator (e.g. a blank line) are ignored.
    }
    if let Some(path) = current_path.take() {
        blocks.push(SourceBlock {
            path,
            content: current_content,
        });
    }
    blocks
}
