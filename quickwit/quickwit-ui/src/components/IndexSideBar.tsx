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

import { Autocomplete, Box, Chip, CircularProgress, IconButton, List, ListItem, ListItemText, TextField, Typography } from '@mui/material';
import React, { useEffect, useMemo, useState } from 'react';
import styled from '@emotion/styled';
import { FieldMapping, getAllFields, IndexMetadata } from '../utils/models';
import { ChevronRight, KeyboardArrowDown } from '@mui/icons-material';
import Tooltip from '@mui/material/Tooltip';
import { Client } from '../services/client';

const IndexBarWrapper = styled('div')({
  display: 'flex',
  height: '100%',
  flex: '0 0 260px',
  maxWidth: '260px',
  flexDirection: 'column',
  borderRight: '1px solid rgba(0, 0, 0, 0.12)',
  overflow: 'auto',
});

function IndexAutocomplete(props: IndexMetadataProps) {
  const [open, setOpen] = React.useState(false);
  const [options, setOptions] = React.useState<readonly IndexMetadata[]>([]);
  const [value, setValue] = React.useState<IndexMetadata | null>(null);
  const [loading, setLoading] = React.useState(false);
  // We want to show the circular progress only if we are loading some results and
  // when there is no option available.
  const showLoading = loading && options.length === 0;
  const quickwitClient = useMemo(() => new Client(), []);

  useEffect(() => {
    if (loading) {
      return;
    }
    setLoading(true);
    quickwitClient.listIndexes().then(
      (indexesMetadata) => {
        setOptions([...indexesMetadata]);
        setLoading(false);
      },
      (error) => {
        console.log("Index autocomplete error", error);
        setLoading(false);
      }
    );
  }, [quickwitClient, open]);

  useEffect(() => {
    if (!open) {
      if (props.indexMetadata !== null && options.length === 0) {
        setOptions([props.indexMetadata]);
      }
    }
  }, [open, props.indexMetadata, options.length]);

  useEffect(() => {
      setValue(props.indexMetadata);
  }, [props.indexMetadata]);

  return (
    <Autocomplete
      size="small"
      sx={{ width: 210 }}
      open={open}
      value={value}
      onChange={(_, updatedValue) => {
        setValue(updatedValue);

        if (updatedValue == null || updatedValue.index_config.index_id == null) {
          props.onIndexMetadataUpdate(null);
        } else {
          props.onIndexMetadataUpdate(updatedValue);
        }
      }}
      onOpen={() => {
        setOpen(true);
      }}
      onClose={() => {
        setOpen(false);
        setLoading(false);
      }}
      isOptionEqualToValue={(option, value) => option.index_config.index_id === value.index_config.index_id}
      getOptionLabel={(option) => option.index_config.index_id}
      options={options}
      noOptionsText="No indexes."
      loading={loading}
      renderInput={(params) => (
        <TextField
          {...params}
          placeholder='Select an index'
          InputProps={{
            ...params.InputProps,
            endAdornment: (
              <React.Fragment>
                {showLoading ? <CircularProgress color="inherit" size={20} /> : null}
                {params.InputProps.endAdornment}
              </React.Fragment>
            ),
          }}
        />
      )}
    />
  );
}

export interface IndexMetadataProps {
  indexMetadata: null | IndexMetadata,
  onIndexMetadataUpdate(indexMetadata: IndexMetadata | null): void;
}

function fieldTypeLabel(fieldMapping: FieldMapping): string {
  if (fieldMapping.type[0] !== undefined) {
    return fieldMapping.type[0].toUpperCase();

  } else {
    return "";
  }
}

export function IndexSideBar(props: IndexMetadataProps) {
  const [open, setOpen] = useState(true);
  const fields = (props.indexMetadata === null) ? [] : getAllFields(props.indexMetadata.index_config.doc_mapping.field_mappings);
  return (
    <IndexBarWrapper>
      <Box sx={{ px: 3, py: 2}}>
        <Typography variant='body1' mb={1}>
          Index ID
        </Typography>
        <IndexAutocomplete { ...props }/>
      </Box>
      <Box sx={{ paddingLeft: "10px", height: '100%'}}>
        <IconButton
            aria-label="expand row"
            size="small"
            onClick={() => setOpen(!open)}
          >
            {open ? <KeyboardArrowDown /> : <ChevronRight />}
        </IconButton>
        Fields
        { open && <List dense={true} sx={{paddingTop: '0', overflowWrap: 'break-word'}}>
          { fields.map(function(field) {
            return <ListItem
              key={ field.json_path }
              secondaryAction={
                <IconButton edge="end" aria-label="add"></IconButton>
              }
              sx={{paddingLeft: '10px'}}
            >
              <Tooltip title={field.field_mapping.type} arrow placement="left">
                <Chip label={fieldTypeLabel(field.field_mapping)} size="small" sx={{marginRight: '10px', borderRadius: '3px', fontSize: '0.6rem'}}/>
              </Tooltip>
              <ListItemText primary={ field.json_path }/>
            </ListItem>
          })}
        </List>
        }
      </Box>
    </IndexBarWrapper>
  );
}
