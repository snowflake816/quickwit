// Copyright (C) 2021 Quickwit, Inc.
//
// Quickwit is offered under the AGPL v3.0 and as commercial software.
// For commercial licensing, contact us at hello@quickwit.io.
//
// AGPL:
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.

use std::env;
use std::fmt::Display;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context};

/// Default file protocol `file://`
const FILE_PROTOCOL: &str = "file";

const PROTOCOL_SEPARATOR: &str = "://";

/// Encapsulates the URI type.
#[derive(Debug, PartialEq, Eq)]
pub struct Uri {
    uri: String,
    protocol_idx: usize,
}

impl Uri {
    /// Tries to to construct a Uri from the raw string.
    /// A `file://` protocol is assumed if not specified.
    /// File URIs are resolved (normalised) relative to the current working directory
    /// unless an absolute path is specified.
    /// Handles special characters like (~, ., ..)
    pub fn try_new(uri: &str) -> anyhow::Result<Self> {
        let (protocol, mut path) = match uri.split_once(PROTOCOL_SEPARATOR) {
            None => (FILE_PROTOCOL, uri.to_string()),
            Some((protocol, path)) => (protocol, path.to_string()),
        };

        if protocol == FILE_PROTOCOL {
            let current_dir =
                env::current_dir().context("Could not fetch the current directory.")?;

            if path.starts_with('~') {
                // We only accept `~` (alias to the home directory) and `~/path/to/something`.
                // If there is something following the `~` that is not `/`, we bail out.
                if path
                    .chars()
                    .nth(1)
                    .map(|second_character| second_character != '/')
                    .unwrap_or(false)
                {
                    bail!("This path syntax `{}` is not supported.", uri);
                }

                let home_dir_path = home::home_dir()
                    .context("Could not fetch the home directory.")?
                    .to_string_lossy()
                    .to_string();

                path.replace_range(0..1, &home_dir_path);
            }

            if !path.starts_with('/') {
                path = current_dir.join(path).to_string_lossy().to_string();
            }

            path = normalize_path(Path::new(&path))
                .to_string_lossy()
                .to_string();
        }

        Ok(Self {
            uri: format!("{}{}{}", protocol, PROTOCOL_SEPARATOR, path),
            protocol_idx: protocol.len(),
        })
    }

    /// Returns the uri protocol.
    pub fn protocol(&self) -> &str {
        &self.uri[..self.protocol_idx]
    }

    /// Returns the file path from the uri.
    /// Useful only for `file://` protocol Uri.
    pub fn filepath(&self) -> Option<&Path> {
        if self.protocol() == "file" {
            self.uri.strip_prefix("file://").map(Path::new)
        } else {
            None
        }
    }
}

impl Display for Uri {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.uri)
    }
}

impl AsRef<str> for Uri {
    fn as_ref(&self) -> &str {
        &self.uri
    }
}

/// Normalizes a path by resolving the components like (., ..).
/// This helper does the same thing as `Path::canonicalize`.
/// It only differs from `Path::canonicalize` by not checking file existence
/// during resolution.
/// https://github.com/rust-lang/cargo/blob/fede83ccf973457de319ba6fa0e36ead454d2e20/src/cargo/util/paths.rs#L61
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = path.components().peekable();
    let mut resulting_path_buf =
        if let Some(component @ Component::Prefix(..)) = components.peek().cloned() {
            components.next();
            PathBuf::from(component.as_os_str())
        } else {
            PathBuf::new()
        };

    for component in components {
        match component {
            Component::Prefix(..) => unreachable!(),
            Component::RootDir => {
                resulting_path_buf.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                resulting_path_buf.pop();
            }
            Component::Normal(inner_component) => {
                resulting_path_buf.push(inner_component);
            }
        }
    }
    resulting_path_buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uri() -> anyhow::Result<()> {
        let home_dir = home::home_dir().unwrap();
        let current_dir = env::current_dir().unwrap();

        let uri = Uri::try_new("file:///home/foo/bar")?;
        assert_eq!(uri.protocol(), "file");
        assert_eq!(uri.filepath(), Some(Path::new("/home/foo/bar")));
        assert_eq!(uri.as_ref(), "file:///home/foo/bar");

        assert_eq!(
            Uri::try_new("home/homer/docs/dognuts")?.to_string(),
            format!("file://{}/home/homer/docs/dognuts", current_dir.display())
        );

        assert_eq!(
            Uri::try_new("home/homer/docs/../dognuts")?.to_string(),
            format!("file://{}/home/homer/dognuts", current_dir.display())
        );

        assert_eq!(
            Uri::try_new("home/homer/docs/../../dognuts")?.to_string(),
            format!("file://{}/home/dognuts", current_dir.display())
        );

        assert_eq!(
            Uri::try_new("/home/homer/docs/dognuts")?.to_string(),
            "file:///home/homer/docs/dognuts"
        );

        assert_eq!(
            Uri::try_new("~")?.to_string(),
            format!("file://{}", home_dir.display())
        );
        assert_eq!(
            Uri::try_new("~/")?.to_string(),
            format!("file://{}", home_dir.display())
        );

        assert_eq!(
            Uri::try_new("~anything/bar").unwrap_err().to_string(),
            "This path syntax `~anything/bar` is not supported."
        );

        assert_eq!(
            Uri::try_new("~/.")?.to_string(),
            format!("file://{}", home_dir.display())
        );
        assert_eq!(
            Uri::try_new("~/..")?.to_string(),
            format!("file://{}", home_dir.parent().unwrap().display())
        );

        assert_eq!(
            Uri::try_new("file://")?.to_string(),
            format!("file://{}", current_dir.display())
        );

        assert_eq!(
            Uri::try_new("file://.")?.to_string(),
            format!("file://{}", current_dir.display())
        );

        assert_eq!(
            Uri::try_new("file://..")?.to_string(),
            format!("file://{}", current_dir.parent().unwrap().display())
        );

        assert_eq!(
            Uri::try_new("s3://home/homer/docs/dognuts")?.to_string(),
            "s3://home/homer/docs/dognuts"
        );

        assert_eq!(
            Uri::try_new("s3://home/homer/docs/../dognuts")?.to_string(),
            "s3://home/homer/docs/../dognuts"
        );

        Ok(())
    }
}
