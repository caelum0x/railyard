# Vend deployment templates

The Kubernetes base follows the repository's existing Kustomize pattern but is
kept product-local until Vend's final domain and cluster are chosen.

Before applying it:

1. replace the image with an immutable digest;
2. replace `https://vend.invalid`, Mint, and Relay service coordinates;
3. create `vend-secrets` through the cluster secret manager using
   `k8s/secret.example.yaml` only as a key inventory;
4. register the resulting GitHub and Google callbacks;
5. size DB connections so `replicas × VEND_DATABASE_MAX_CONNECTIONS` stays
   below the pooler/database budget;
6. expose only Service port 80 through the chosen TLS ingress, while keeping
   `/metrics` private.

Render without changing a cluster:

```bash
kubectl kustomize apps/vend/deploy/k8s
```

The Prometheus rule uses the Prometheus Operator CRD. Install it only where
that CRD and `kube-state-metrics` exist; otherwise translate the expressions
into the environment's alert manager.
