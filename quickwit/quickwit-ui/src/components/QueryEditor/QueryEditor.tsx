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

import { useEffect, useRef, useState } from 'react';
import MonacoEditor from 'react-monaco-editor';
import * as monacoEditor from 'monaco-editor/esm/vs/editor/editor.api';
import { LANGUAGE_CONFIG, LanguageFeatures, createIndexCompletionProvider } from './config';
import { SearchComponentProps } from '../../utils/SearchComponentProps';
import { EDITOR_THEME } from '../../utils/theme';
import { Box } from '@mui/material';

const QUICKWIT_EDITOR_THEME_ID = 'quickwit-light';

function getLanguageId(indexId: string | null): string {
  if (indexId === null) {
    return '';
  }
  return `${indexId}-query-language`;
}

export function QueryEditor(props: SearchComponentProps) {
  const monacoRef = useRef<null | typeof monacoEditor>(null);
  const [languageId, setLanguageId] = useState<string>('');
  const runSearchRef = useRef(props.runSearch);
  const searchRequestRef = useRef(props.searchRequest);
  const defaultValue = props.searchRequest.query === null ? `// Select an index and type your query. Example: field_name:"phrase query"` : props.searchRequest.query;

  /* eslint-disable  @typescript-eslint/no-explicit-any */
  function handleEditorDidMount(editor: any, monaco: any) {
    monacoRef.current = monaco;
    editor.addAction({
      id: 'SEARCH',
      label: "Run search",
      keybindings: [
        monaco.KeyCode.F9,
        monaco.KeyMod.CtrlCmd | monaco.KeyCode.Enter,
      ],
      run: () => {
        runSearchRef.current(searchRequestRef.current);
      },
    })
  }

  useEffect(() => {
    const updatedLanguageId = getLanguageId(props.searchRequest.indexId);
    if (monacoRef.current !== null && updatedLanguageId !== '' && props.index !== null) {
      const monaco = monacoRef.current;
      if (!monaco.languages.getLanguages().some(({ id }: {id :string }) => id === updatedLanguageId)) {
        console.log('register language', updatedLanguageId);
        monaco.languages.register({'id': updatedLanguageId});
        monaco.languages.setMonarchTokensProvider(updatedLanguageId, LanguageFeatures())
        if (props.index != null) {
          monaco.languages.registerCompletionItemProvider(updatedLanguageId, createIndexCompletionProvider(props.index.metadata));
          monaco.languages.setLanguageConfiguration(
            updatedLanguageId,
            LANGUAGE_CONFIG,
          );
        }
      }
      setLanguageId(updatedLanguageId);
    }
  }, [monacoRef, props.index]);

  useEffect(() => {
    if (monacoRef.current !== null) {
      runSearchRef.current = props.runSearch;
    }
  }, [monacoRef, props.runSearch]);

  function handleEditorChange(value: any) {
    const updatedSearchRequest = Object.assign({}, props.searchRequest, {query: value});
    searchRequestRef.current = updatedSearchRequest;
    props.onSearchRequestUpdate(updatedSearchRequest);
  }

  function handleEditorWillMount(monaco: any) {
    monaco.editor.defineTheme(QUICKWIT_EDITOR_THEME_ID, EDITOR_THEME);
  }

  return (
    <Box sx={{ height: '100px', py: 1}} >
      <MonacoEditor
        editorWillMount={handleEditorWillMount}
        editorDidMount={handleEditorDidMount}
        onChange={handleEditorChange}
        language={languageId}
        value={defaultValue}
        options={{
          fontFamily: 'monospace',
          minimap: {
            enabled: false,
          },
          renderLineHighlight: "gutter",
          fontSize: 14,
          fixedOverflowWidgets: true,
          scrollBeyondLastLine: false,
      }}
      theme={QUICKWIT_EDITOR_THEME_ID}
      />
    </Box>
  );
}
