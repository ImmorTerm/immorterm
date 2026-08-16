# Warm self-hosted CI runner (ARC on the flam k3s cluster)

One always-warm runner keeps the checkout, `node_modules`, `target/` and the
cargo registry between runs. Measured baseline on hosted runners: PR CI wall
~87s warm, 5m20s on a cold cargo cache. The warm runner removes the install,
toolchain setup and cache restore/save from every run.

CI needs nothing from this to work: with the `FAST_RUNNER_LABEL` repository
variable unset, every workflow runs on `ubuntu-latest` exactly as before.
Unset the variable during a cluster incident and CI falls straight back.

## Install (once, needs cluster write access)

```bash
export KUBECONFIG=~/.kube/flam-memory.yaml

# 1. Build + push the runner image (Dockerfile has the exact commands):
#    .devops/docker/Dockerfile.ci-runner

# 2. Label the cache-owner node — exactly one, and NOT flam-memory-2
#    (FLAM's CI caches live there; two builds on one 4-CPU node starve
#    each other):
kubectl label node flam-memory-1 immorterm.dev/ci-cache=true

# 3. Install the scale set (same chart + controller the flam-cluster and
#    longstory-runner tenants already use; arc-systems runs the controller):
helm install immorterm \
  oci://ghcr.io/actions/actions-runner-controller-charts/gha-runner-scale-set \
  --version 0.14.0 \
  --namespace immorterm-actions-runners --create-namespace \
  --set githubConfigUrl=https://github.com/ImmorTerm/immorterm \
  --set githubConfigSecret=immorterm-runner-github-app \
  --set minRunners=1 --set maxRunners=1 \
  -f .devops/k8s/immorterm-runner-values.yaml
# (create the immorterm-runner-github-app secret the same way as FLAM's —
#  a GitHub App with Actions runner scopes, or a PAT secret per ARC docs)

# 4. Point CI at the pool + arm the queue alarm:
gh variable set FAST_RUNNER_LABEL --repo ImmorTerm/immorterm --body immorterm
gh secret set ALERT_WEBHOOK_URL --repo ImmorTerm/immorterm   # Slack webhook
```

## When the queue is stuck

`ci-queue-alarm.yml` posts to Slack when runs sit queued > 10 min. First look:

```bash
kubectl -n immorterm-actions-runners get pods
kubectl -n arc-systems get pods        # the ARC controller itself
```

A dead runner fails SILENTLY — jobs queue forever and no check goes red.
Fastest unblock is always: `gh variable delete FAST_RUNNER_LABEL` → CI falls
back to hosted runners on the next run.
