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

import { Typography } from '@mui/material';
import { useEffect, useMemo, useState } from 'react';
import ApiUrlFooter from '../components/ApiUrlFooter';
import { JsonEditor } from '../components/JsonEditor';
import { ViewUnderAppBarBox, FullBoxContainer, QBreadcrumbs } from '../components/LayoutUtils';
import Loader from '../components/Loader';
import { Client } from '../services/client';
import { Member } from '../utils/models';
import { ErrorResult } from './IndexesView';


function ClusterView() {
  const [loading, setLoading] = useState(false);
  const [members, setMembers] = useState<null | Member[]>(null);
  const [ , setLoadingError] = useState<ErrorResult | null>(null);
  const quickwitClient = useMemo(() => new Client(), []);

  useEffect(() => {
    setLoading(true);
    quickwitClient.clusterMembers().then(
      (clusterMembers) => {
        setLoadingError(null);
        setLoading(false);
        setMembers(clusterMembers.members);
      },
      (error) => {
        setLoading(false);
        setLoadingError({error: error});
      }
    );
  }, [quickwitClient]);

  const renderResult = () => {
    if (loading || members == null) {
      return <Loader />;
    } else {
      return <JsonEditor content={members} resizeOnMount={false} />
    }
  }

  return (
    <ViewUnderAppBarBox>
      <FullBoxContainer>
        <QBreadcrumbs aria-label="breadcrumb">
          <Typography color="text.primary">Cluster members</Typography>
        </QBreadcrumbs>
        <FullBoxContainer sx={{ px: 0 }}>
          { renderResult() }
        </FullBoxContainer>
      </FullBoxContainer>
      { ApiUrlFooter('api/v1/cluster/members') }
    </ViewUnderAppBarBox>
  );
}

export default ClusterView;
