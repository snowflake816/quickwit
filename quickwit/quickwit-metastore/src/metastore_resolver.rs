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

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use once_cell::sync::OnceCell;
use quickwit_common::uri::{Protocol, Uri};

use crate::metastore::file_backed_metastore::FileBackedMetastoreFactory;
#[cfg(feature = "postgres")]
use crate::metastore::postgresql_metastore::PostgresqlMetastoreFactory;
use crate::{Metastore, MetastoreResolverError};

/// A metastore factory builds a [`Metastore`] object from an URI.
#[cfg_attr(any(test, feature = "testsuite"), mockall::automock)]
#[async_trait]
pub trait MetastoreFactory: Send + Sync + 'static {
    /// Returns the appropriate [`Metastore`] object for the URI.
    async fn resolve(&self, uri: &Uri) -> Result<Arc<dyn Metastore>, MetastoreResolverError>;
}

#[derive(Default)]
pub struct MetastoreUriResolverBuilder {
    per_protocol_resolver: HashMap<Protocol, Arc<dyn MetastoreFactory>>,
}

impl MetastoreUriResolverBuilder {
    pub fn register<S: MetastoreFactory>(mut self, protocol: Protocol, resolver: S) -> Self {
        self.per_protocol_resolver
            .insert(protocol, Arc::new(resolver));
        self
    }

    pub fn build(self) -> MetastoreUriResolver {
        MetastoreUriResolver {
            per_protocol_resolver: Arc::new(self.per_protocol_resolver),
        }
    }
}

/// Resolves an URI by dispatching it to the right [`MetastoreFactory`]
/// based on its protocol.
pub struct MetastoreUriResolver {
    per_protocol_resolver: Arc<HashMap<Protocol, Arc<dyn MetastoreFactory>>>,
}

/// Quickwit supported storage resolvers.
///
/// The returned metastore uri resolver is a Singleton.
pub fn quickwit_metastore_uri_resolver() -> &'static MetastoreUriResolver {
    static METASTORE_URI_RESOLVER: OnceCell<MetastoreUriResolver> = OnceCell::new();
    METASTORE_URI_RESOLVER.get_or_init(|| {
        #[allow(unused_mut)]
        let mut builder = MetastoreUriResolver::builder()
            .register(Protocol::Ram, FileBackedMetastoreFactory::default())
            .register(Protocol::File, FileBackedMetastoreFactory::default())
            .register(Protocol::S3, FileBackedMetastoreFactory::default());

        #[cfg(feature = "postgres")]
        {
            builder = builder.register(Protocol::PostgreSQL, PostgresqlMetastoreFactory::default());
        }

        #[cfg(not(feature = "postgres"))]
        {
            builder = builder.register(
                Protocol::PostgreSQL,
                UnsupportedMetastore {
                    message: "postgres unsupported, quickwit was compiled without the 'postgres' \
                              feature flag"
                        .to_string(),
                },
            )
        }

        #[cfg(feature = "azure")]
        {
            builder = builder.register(Protocol::Azure, FileBackedMetastoreFactory::default());
        }

        #[cfg(not(feature = "azure"))]
        {
            builder = builder.register(
                Protocol::Azure,
                UnsupportedMetastore {
                    message: "azure unsupported, quickwit was compiled without the `azure` \
                              feature flag"
                        .to_string(),
                },
            )
        }

        builder.build()
    })
}

/// A metastore factory for handling unsupported metastore.
#[derive(Clone, Default)]
pub struct UnsupportedMetastore {
    message: String,
}

#[async_trait]
impl MetastoreFactory for UnsupportedMetastore {
    async fn resolve(&self, _uri: &Uri) -> Result<Arc<dyn Metastore>, MetastoreResolverError> {
        Err(MetastoreResolverError::ProtocolUnsupported(
            self.message.to_string(),
        ))
    }
}

impl MetastoreUriResolver {
    /// Creates an empty `MetastoreUriResolver`.
    pub fn builder() -> MetastoreUriResolverBuilder {
        MetastoreUriResolverBuilder::default()
    }

    /// Resolves the given URI.
    pub async fn resolve(&self, uri: &Uri) -> Result<Arc<dyn Metastore>, MetastoreResolverError> {
        let resolver = self
            .per_protocol_resolver
            .get(&uri.protocol())
            .ok_or_else(|| {
                MetastoreResolverError::ProtocolUnsupported(uri.protocol().to_string())
            })?;
        let metastore = resolver.resolve(uri).await?;
        Ok(metastore)
    }
}

#[cfg(test)]
mod tests {
    use quickwit_common::uri::Uri;

    use crate::quickwit_metastore_uri_resolver;

    #[tokio::test]
    async fn test_metastore_resolver_should_not_raise_errors_on_file() -> anyhow::Result<()> {
        let metastore_resolver = quickwit_metastore_uri_resolver();
        metastore_resolver
            .resolve(&Uri::from_well_formed("file://"))
            .await?;
        Ok(())
    }

    #[cfg(feature = "postgres")]
    #[tokio::test]
    async fn test_postgres_and_postgresql_protocol_accepted() {
        use std::env;
        let metastore_resolver = quickwit_metastore_uri_resolver();
        // If the database defined in the env var or the default one is not up, the
        // test block after making 10 attempts with a timeout of 10s each = 100s.
        let test_database_url = env::var("TEST_DATABASE_URL").unwrap_or_else(|_| {
            "postgres://quickwit-dev:quickwit-dev@localhost/quickwit-metastore-dev".to_string()
        });
        let (_uri_protocol, uri_path) = test_database_url.split_once("://").unwrap();
        for protocol in &["postgres", "postgresql"] {
            let postgres_uri = Uri::from_well_formed(format!("{protocol}://{uri_path}"));
            metastore_resolver.resolve(&postgres_uri).await.unwrap();
        }
    }
}
