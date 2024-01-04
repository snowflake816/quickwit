// Copyright (C) 2024 Quickwit, Inc.
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

use anyhow::{bail, Context};
use clap::{arg, Arg, ArgAction, ArgMatches, Command};
use tracing::Level;

use crate::index::{build_index_command, IndexCliCommand};
use crate::service::{build_run_command, RunCliCommand};
use crate::source::{build_source_command, SourceCliCommand};
use crate::split::{build_split_command, SplitCliCommand};
use crate::tool::{build_tool_command, ToolCliCommand};

pub fn build_cli() -> Command {
    Command::new("Quickwit")
        .arg(
            // Following https://no-color.org/
            Arg::new("no-color")
                .long("no-color")
                .help(
                    "Disable ANSI terminal codes (colors, etc...) being injected into the logging \
                     output",
                )
                .env("NO_COLOR")
                .value_parser(clap::builder::FalseyValueParser::new())
                .global(true)
                .action(ArgAction::SetTrue),
        )
        .arg(arg!(-y --"yes" "Assume \"yes\" as an answer to all prompts and run non-interactively.")
            .global(true)
            .required(false)
        )
        .subcommand(build_run_command().display_order(1))
        .subcommand(build_index_command().display_order(2))
        .subcommand(build_source_command().display_order(3))
        .subcommand(build_split_command().display_order(4))
        .subcommand(build_tool_command().display_order(5))
        .arg_required_else_help(true)
        .disable_help_subcommand(true)
        .subcommand_required(true)
}

#[derive(Debug, PartialEq)]
pub enum CliCommand {
    Run(RunCliCommand),
    Index(IndexCliCommand),
    Split(SplitCliCommand),
    Source(SourceCliCommand),
    Tool(ToolCliCommand),
}

impl CliCommand {
    pub fn default_log_level(&self) -> Level {
        match self {
            CliCommand::Run(_) => Level::INFO,
            CliCommand::Index(subcommand) => subcommand.default_log_level(),
            CliCommand::Source(_) => Level::ERROR,
            CliCommand::Split(_) => Level::ERROR,
            CliCommand::Tool(_) => Level::ERROR,
        }
    }

    pub fn parse_cli_args(mut matches: ArgMatches) -> anyhow::Result<Self> {
        let (subcommand, submatches) = matches
            .remove_subcommand()
            .context("failed to parse command")?;
        match subcommand.as_str() {
            "index" => IndexCliCommand::parse_cli_args(submatches).map(CliCommand::Index),
            "run" => RunCliCommand::parse_cli_args(submatches).map(CliCommand::Run),
            "source" => SourceCliCommand::parse_cli_args(submatches).map(CliCommand::Source),
            "split" => SplitCliCommand::parse_cli_args(submatches).map(CliCommand::Split),
            "tool" => ToolCliCommand::parse_cli_args(submatches).map(CliCommand::Tool),
            _ => bail!("unknown command `{subcommand}`"),
        }
    }

    pub async fn execute(self) -> anyhow::Result<()> {
        match self {
            CliCommand::Index(subcommand) => subcommand.execute().await,
            CliCommand::Run(subcommand) => subcommand.execute().await,
            CliCommand::Source(subcommand) => subcommand.execute().await,
            CliCommand::Split(subcommand) => subcommand.execute().await,
            CliCommand::Tool(subcommand) => subcommand.execute().await,
        }
    }
}
