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

import { useEffect, useMemo, useRef, useState } from 'react';
import { useLocation, useNavigate } from 'react-router-dom';
import ApiUrlFooter from '../components/ApiUrlFooter';
import { IndexSideBar } from '../components/IndexSideBar';
import { ViewUnderAppBarBox, FullBoxContainer } from '../components/LayoutUtils';
import { QueryEditorActionBar } from '../components/QueryActionBar';
import { QueryEditor } from '../components/QueryEditor/QueryEditor';
import SearchResult from '../components/SearchResult/SearchResult';
import { useLocalStorage } from '../providers/LocalStorageProvider';
import { Client } from '../services/client';
import { EMPTY_SEARCH_REQUEST, Index, IndexMetadata, ResponseError, SearchRequest, SearchResponse } from '../utils/models';
import { hasSearchParams, parseSearchUrl, toUrlSearchRequestParams } from '../utils/urls';

function updateSearchRequestWithIndex(index: Index | null, searchRequest: SearchRequest) {
  // If we have a timestamp field, order by desc on the timestamp field.
  if (index?.metadata.index_config.doc_mapping.timestamp_field) {
    searchRequest.sortByField = {
      field_name: index?.metadata.index_config.doc_mapping.timestamp_field,
      order: 'Desc'
    };
  } else {
    searchRequest.sortByField = null;
    searchRequest.startTimestamp = null;
    searchRequest.endTimestamp = null;
  }
  if (index?.metadata.index_config.index_id) {
    searchRequest.indexId = index?.metadata.index_config.index_id;
  }
}

function SearchView() {
  const location = useLocation();
  const navigate = useNavigate();
  const [index, setIndex] = useState<null | Index>(null);
  const prevIndexIdRef = useRef<string | null>();
  const [searchResponse, setSearchResponse] = useState<null | SearchResponse>(null);
  const [searchError, setSearchError] = useState<null | ResponseError>(null);
  const [queryRunning, setQueryRunning] = useState(false);
  const [searchRequest, setSearchRequest] = useState<SearchRequest>(hasSearchParams(location.search) ? parseSearchUrl(location.search) : EMPTY_SEARCH_REQUEST);
  const updateLastSearchRequest = useLocalStorage().updateLastSearchRequest;
  const quickwitClient = useMemo(() => new Client(), []);

  const runSearch = (updatedSearchRequest: SearchRequest) => {
    if (!updatedSearchRequest || !updatedSearchRequest.indexId) {
      return;
    }

    console.log('Run search...', updatedSearchRequest);
    updateSearchRequestWithIndex(index, updatedSearchRequest);
    setSearchRequest(updatedSearchRequest);
    setQueryRunning(true);
    setSearchError(null);
    navigate('/search?' + toUrlSearchRequestParams(updatedSearchRequest).toString());
    quickwitClient.search(updatedSearchRequest).then((response) => {
      updateLastSearchRequest(updatedSearchRequest);
      setSearchResponse(response);
      setQueryRunning(false);
    }, (error) => {
      setQueryRunning(false);
      setSearchError(error);
      console.error('Error when running search request', error);
    });
  }
  const onIndexMetadataUpdate = (indexMetadata: IndexMetadata | null) => {
    setSearchRequest(previousRequest => {
      updateSearchRequestWithIndex(index, previousRequest);
      return {...previousRequest, indexId: indexMetadata === null ? null : indexMetadata.index_config.index_id};
    });
  }
  const onSearchRequestUpdate = (searchRequest: SearchRequest) => {
    console.log("on search request update:", searchRequest);
    setSearchRequest(searchRequest);
  }
  useEffect(() => {
    if (prevIndexIdRef.current !== index?.metadata.index_config.index_id) {
      setSearchResponse(null);
    }
    // Run search only if this is the first time we set the index.
    if (prevIndexIdRef.current === null) {
      runSearch(searchRequest);
    }
    prevIndexIdRef.current = index === null ? null : index.metadata.index_config.index_id;
  }, [index]);
  useEffect(() => {
    if (!searchRequest.indexId) {
      return;
    }

    if (index !== null && index.metadata.index_config.index_id === searchRequest.indexId) {
      return;
    }
    // If index id is changing, it's better to reset timestamps as the time unit may be different
    // between indexes.
    if (prevIndexIdRef.current !== null && prevIndexIdRef.current !== index?.metadata.index_config.index_id) {
      searchRequest.startTimestamp = null;
      searchRequest.endTimestamp = null;
    }
    quickwitClient.getIndex(searchRequest.indexId).then((fetchedIndex) => {
      setIndex(fetchedIndex);
    });
  }, [searchRequest, quickwitClient, index]);

  const searchParams = toUrlSearchRequestParams(searchRequest);
  // `toUrlSearchRequestParams` is used for the UI urls. We need to remove the `indexId` request parameter to generate
  // the correct API url, this is the only difference.
  searchParams.delete('index_id');
  return (
      <ViewUnderAppBarBox sx={{ flexDirection: 'row'}}>
        <IndexSideBar indexMetadata={index === null ? null : index.metadata} onIndexMetadataUpdate={onIndexMetadataUpdate}/>
        <FullBoxContainer sx={{ padding: 0}}>
          <FullBoxContainer>
            <QueryEditorActionBar
              searchRequest={searchRequest}
              onSearchRequestUpdate={onSearchRequestUpdate}
              runSearch={runSearch}
              index={index}
              queryRunning={queryRunning} />
            <QueryEditor
              searchRequest={searchRequest}
              onSearchRequestUpdate={onSearchRequestUpdate}
              runSearch={runSearch}
              index={index}
              queryRunning={queryRunning} />
            <SearchResult
              queryRunning={queryRunning}
              searchError={searchError}
              searchResponse={searchResponse}
              index={index} />
          </FullBoxContainer>
          { index !== null && ApiUrlFooter(`api/v1/${index?.metadata.index_config.index_id}/search?${searchParams.toString()}`) }
        </FullBoxContainer>
      </ViewUnderAppBarBox>
  );
}

export default SearchView;
