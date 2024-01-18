import aws_cdk
from aws_cdk import Stack, aws_s3_assets
from constructs import Construct
import yaml

from ..services import quickwit_service


INDEX_STORE_BUCKET_NAME_EXPORT_NAME = "hdfs-index-store-bucket-name"
INDEXER_FUNCTION_NAME_EXPORT_NAME = "hdfs-indexer-function-name"
SEARCHER_FUNCTION_NAME_EXPORT_NAME = "hdfs-searcher-function-name"


class HdfsStack(Stack):
    def __init__(
        self,
        scope: Construct,
        construct_id: str,
        indexer_memory_size: int,
        searcher_memory_size: int,
        indexer_package_location: str,
        searcher_package_location: str,
        **kwargs
    ) -> None:
        super().__init__(scope, construct_id, **kwargs)

        index_config_local_path = "./resources/hdfs-logs.yaml"

        with open(index_config_local_path) as f:
            index_config_dict = yaml.safe_load(f)
            index_id = index_config_dict["index_id"]

        index_config = aws_s3_assets.Asset(
            self,
            "mock-data-index-config",
            path=index_config_local_path,
        )
        lambda_env = {
            **quickwit_service.extract_local_env(),
            "RUST_LOG": "quickwit=debug",
        }
        qw_svc = quickwit_service.QuickwitService(
            self,
            "Quickwit",
            index_id=index_id,
            index_config_bucket=index_config.s3_bucket_name,
            index_config_key=index_config.s3_object_key,
            indexer_environment=lambda_env,
            searcher_environment=lambda_env,
            indexer_memory_size=indexer_memory_size,
            searcher_memory_size=searcher_memory_size,
            indexer_package_location=indexer_package_location,
            searcher_package_location=searcher_package_location,
        )

        aws_cdk.CfnOutput(
            self,
            "index-store-bucket-name",
            value=qw_svc.bucket.bucket_name,
            export_name=INDEX_STORE_BUCKET_NAME_EXPORT_NAME,
        )
        aws_cdk.CfnOutput(
            self,
            "indexer-function-name",
            value=qw_svc.indexer.lambda_function.function_name,
            export_name=INDEXER_FUNCTION_NAME_EXPORT_NAME,
        )
        aws_cdk.CfnOutput(
            self,
            "searcher-function-name",
            value=qw_svc.searcher.lambda_function.function_name,
            export_name=SEARCHER_FUNCTION_NAME_EXPORT_NAME,
        )
