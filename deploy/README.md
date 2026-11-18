# Deploying bdc-search

`bdc-search.yaml` runs the search API on OpenShift (UNC Cloud Apps): one Deployment, a Service
and a public HTTPS Route. The API key is the access control (see "Search API" in the top-level
README); the health probes stay open.

It currently runs in the `kschaper` project as a temporary home, at
https://bdc-search-kschaper.apps.cloudapps.unc.edu.

## First deploy

```sh
oc login --server=https://api.cloudapps.unc.edu:6443   # token from console.apps.cloudapps.unc.edu
oc project kschaper

# the API key, read from a file so it stays out of shell history
openssl rand -hex 32 | tr -d '\n' > api-key.txt
oc create secret generic bdc-search --from-file=api-key=api-key.txt

oc apply -f deploy/bdc-search.yaml
```

## A new KG release

The index is baked into the image, so a release is a new image tag:

```sh
just search-release release=YYYY-MM-DD      # after the KG build
just search-image
just search-image-release YYYY-MM-DD

R=default-route-openshift-image-registry.apps.cloudapps.unc.edu
oc whoami -t | docker login -u "$(oc whoami)" --password-stdin $R
docker tag bdc-search:YYYY-MM-DD $R/kschaper/bdc-search:YYYY-MM-DD
docker push $R/kschaper/bdc-search:YYYY-MM-DD

oc set image deploy/bdc-search bdc-search=image-registry.openshift-image-registry.svc:5000/kschaper/bdc-search:YYYY-MM-DD
oc rollout status deploy/bdc-search
```

Then update the image tag in `bdc-search.yaml` to match. `/version` reports the release being
served.

## Rotating the key

```sh
openssl rand -hex 32 | tr -d '\n' > api-key.txt
oc create secret generic bdc-search --from-file=api-key=api-key.txt --dry-run=client -o yaml | oc apply -f -
oc rollout restart deploy/bdc-search
```
