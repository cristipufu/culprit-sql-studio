//! Explicit local-file intake does not open a workspace, source connection or telemetry destination.
use crate::{
    bounded_output,
    output::{Format, Out, observations},
};
use clap::{Args, ValueEnum};
use culprit_core::{Error, NativeLogFormat, NativeLogLimits, Result, t};
use std::{io::Write, path::PathBuf};

#[derive(Args)]
pub(crate) struct LogsArgs {
    #[arg(value_name="FILE",help=culprit_core::t!("native_logs.cli.file"))]
    file: PathBuf,
    #[arg(long,value_enum,help=culprit_core::t!("native_logs.cli.input_format"))]
    input_format: InputFormat,
    #[arg(long,default_value_t=NativeLogLimits::default().input_bytes,help=culprit_core::t!("native_logs.cli.input_bytes"))]
    max_input_bytes: usize,
    #[arg(long,default_value_t=NativeLogLimits::default().record_bytes,help=culprit_core::t!("native_logs.cli.record_bytes"))]
    max_record_bytes: usize,
    #[arg(long,default_value_t=NativeLogLimits::default().field_bytes,help=culprit_core::t!("native_logs.cli.field_bytes"))]
    max_field_bytes: usize,
    #[arg(long,default_value_t=NativeLogLimits::default().records,help=culprit_core::t!("native_logs.cli.records"))]
    max_records: usize,
    #[arg(long,default_value_t=bounded_output::DEFAULT_JSON_BYTES,help=culprit_core::t!("native_logs.cli.output_bytes"))]
    max_output_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum InputFormat {
    Json,
    Csv13,
    Csv,
}
impl From<InputFormat> for NativeLogFormat {
    fn from(format: InputFormat) -> Self {
        match format {
            InputFormat::Json => Self::PostgresJson,
            InputFormat::Csv13 => Self::PostgresCsv13,
            InputFormat::Csv => Self::PostgresCsv,
        }
    }
}

impl LogsArgs {
    fn limits(&self, format: Format) -> Result<NativeLogLimits> {
        if !matches!(format, Format::Text | Format::Json) {
            return Err(Error::invalid(t!("native_logs.cli.format")));
        }
        bounded_output::validate_json_limit(self.max_output_bytes)?;
        let limits = NativeLogLimits {
            input_bytes: self.max_input_bytes,
            record_bytes: self.max_record_bytes,
            field_bytes: self.max_field_bytes,
            records: self.max_records,
        };
        if limits.is_valid() { Ok(limits) } else { Err(Error::invalid(t!("native_logs.invalid_limits"))) }
    }
}

pub(crate) async fn run(args: LogsArgs, out: &Out) -> Result<u8> {
    let limits = args.limits(out.format())?;
    let output_limit = args.max_output_bytes;
    let analysis = culprit_engine::run_blocking(move || {
        culprit_engine::native_logs::analyze_file(&args.file, args.input_format.into(), limits)
    })
    .await?;
    if out.format() == Format::Json {
        let bytes = bounded_output::json(&analysis, output_limit)?;
        std::io::stdout().lock().write_all(&bytes).map_err(|_| Error::internal(t!("cli.output_failed")))?;
    } else {
        println!("{}", observations::retained(&analysis.report));
        if !analysis.is_read_complete() {
            println!("{}", t!("native_logs.cli.incomplete"));
        }
    }
    Ok(if analysis.is_read_complete() { 0 } else { 2 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Command {
        #[command(flatten)]
        args: LogsArgs,
    }

    #[test]
    fn native_layout_must_be_explicit_and_does_not_conflict_with_global_output_format() {
        assert!(Command::try_parse_from(["logs", "input.json"]).is_err());
        for (name, expected) in [("json", InputFormat::Json), ("csv13", InputFormat::Csv13), ("csv", InputFormat::Csv)]
        {
            let command = Command::try_parse_from(["logs", "not-an-identity.csv", "--input-format", name]).unwrap();
            assert_eq!(command.args.input_format, expected);
            assert!(command.args.limits(Format::Json).is_ok());
            assert!(command.args.limits(Format::Sarif).is_err());
        }
    }

    #[test]
    fn invalid_input_or_output_limits_fail_before_file_intake() {
        for options in [vec!["--max-input-bytes", "0"], vec!["--max-records", "4097"], vec!["--max-output-bytes", "1"]]
        {
            let mut args = vec!["logs", "nonexistent-private-input", "--input-format", "json"];
            args.extend(options);
            let command = Command::try_parse_from(args).unwrap();
            assert!(command.args.limits(Format::Json).is_err());
        }
    }
}
