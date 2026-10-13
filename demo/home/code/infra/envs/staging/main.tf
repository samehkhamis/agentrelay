module "cluster" {
  source    = "../../modules/cluster"
  name      = "staging"
  node_pool = "general-v2"
  nodes     = 3
}
