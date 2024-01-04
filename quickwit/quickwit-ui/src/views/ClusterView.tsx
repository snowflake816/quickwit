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

import { Typography } from '@mui/material';
import { useEffect, useMemo, useState } from 'react';
import ApiUrlFooter from '../components/ApiUrlFooter';
import { JsonEditor } from '../components/JsonEditor';
import { ViewUnderAppBarBox, FullBoxContainer, QBreadcrumbs } from '../components/LayoutUtils';
import Loader from '../components/Loader';
import ErrorResponseDisplay from '../components/ResponseErrorDisplay';
import { Client } from '../services/client';
import { Cluster, ResponseError } from '../utils/models';


function ClusterView() {
  const [loading, setLoading] = useState(false);
  const [cluster, setCluster] = useState<null | Cluster>(null);
  const [responseError, setResponseError] = useState<ResponseError | null>(null);
  const quickwitClient = useMemo(() => new Client(), []);

  useEffect(() => {
    setLoading(true);
    quickwitClient.cluster().then(
      (cluster) => {
        setResponseError(null);
        setLoading(false);
        setCluster(cluster);
      },
      (error) => {
        setLoading(false);
        setResponseError(error);
      }
    );
  }, [quickwitClient]);

  const renderResult = () => {
    if (responseError !== null) {
      return ErrorResponseDisplay(responseError);
    }
    if (loading || cluster == null) {
      return <Loader />;
    }
    return <JsonEditor content={cluster} resizeOnMount={false} />
  }

  return (
    <ViewUnderAppBarBox>
      <FullBoxContainer>
        <QBreadcrumbs aria-label="breadcrumb">
          <Typography color="text.primary">Cluster</Typography>
        </QBreadcrumbs>
        <FullBoxContainer sx={{ px: 0 }}>
          { renderResult() }
        </FullBoxContainer>
      </FullBoxContainer>
      { ApiUrlFooter('api/v1/cluster') }
    </ViewUnderAppBarBox>
  );
}

export default ClusterView;
