# install-demo-kubernetes

An example speedyspec plugin: it makes `speedyspec` able to install a small demo app into a
Kubernetes cluster.

It registers one app, `demo`, and answers the Kubernetes driver with three resources: a
ConfigMap, a Deployment of one replica of `registry.k8s.io/pause`, and a Service. The CLI
connects to the cluster, creates the namespace if it is missing, records the install, applies
the resources, and waits for the Deployment; the plugin only says what should exist.

Run it straight from a checkout, against the cluster your kubeconfig points at:

```bash
speedyspec --plugin-dir examples/install-demo-kubernetes app install demo --namespace demo
```

Or name the cluster and give a token (never as a flag value: in the environment or a file):

```bash
SPEEDYSPEC_KUBE_TOKEN=... speedyspec --plugin-dir examples/install-demo-kubernetes app install demo --namespace demo --server https://cluster.example:6443
```

Remove it again, with no plugin needed:

```bash
speedyspec app uninstall demo --namespace demo
```
