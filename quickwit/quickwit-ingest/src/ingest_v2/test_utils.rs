// Copyright (C) 2023 Quickwit, Inc.
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

use std::ops::RangeBounds;

use mrecordlog::MultiRecordLog;

pub(super) trait MultiRecordLogTestExt {
    fn assert_records_eq<R>(&self, queue_id: &str, range: R, expected_records: &[(u64, &str)])
    where R: RangeBounds<u64> + 'static;
}

impl MultiRecordLogTestExt for MultiRecordLog {
    #[track_caller]
    fn assert_records_eq<R>(&self, queue_id: &str, range: R, expected_records: &[(u64, &str)])
    where R: RangeBounds<u64> + 'static {
        let records = self
            .range(queue_id, range)
            .unwrap()
            .map(|(position, record)| (position, String::from_utf8(record.into_owned()).unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            records.len(),
            expected_records.len(),
            "expected {} records, got {}",
            expected_records.len(),
            records.len()
        );
        for ((position, record), (expected_position, expected_record)) in
            records.iter().zip(expected_records.iter())
        {
            assert_eq!(
                position, expected_position,
                "expected record at position `{expected_position}`, got `{position}`",
            );
            assert_eq!(
                record, expected_record,
                "expected record `{expected_record}`, got `{record}`",
            );
        }
    }
}
