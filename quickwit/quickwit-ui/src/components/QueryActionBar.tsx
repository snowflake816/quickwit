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

import { Box, Button } from "@mui/material";
import { TimeRangeSelect } from './TimeRangeSelect';
import PlayArrowIcon from '@mui/icons-material/PlayArrow';
import { SearchComponentProps } from "../utils/SearchComponentProps";

export function QueryEditorActionBar(props: SearchComponentProps) {
  const timestamp_field_name = props.index?.metadata.index_config.doc_mapping.timestamp_field;
  const shouldDisplayTimeRangeSelect = timestamp_field_name ?? false;
  return (
    <Box sx={{ display: 'flex'}}>
      <Box sx={{ flexGrow: 1 }}>
        <Button
          onClick={() => props.runSearch(props.searchRequest)}
          variant="contained"
          startIcon={<PlayArrowIcon />}
          disableElevation
          sx={{ flexGrow: 1}}
          disabled={props.queryRunning || !props.searchRequest.indexId}>
          Run
        </Button>
      </Box>
      { shouldDisplayTimeRangeSelect && <TimeRangeSelect
          timeRange={{
            startTimestamp:props.searchRequest.startTimestamp,
            endTimestamp:props.searchRequest.endTimestamp
          }}
          onUpdate={
            (timeRange)=>{
              props.runSearch({...props.searchRequest, ...timeRange});
            }
          }
          disabled={props.queryRunning || !props.searchRequest.indexId}
       />
      }
    </Box>
  )
}
