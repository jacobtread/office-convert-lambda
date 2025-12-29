# Office Convert Lambda

![License](https://img.shields.io/github/license/jacobtread/office-convert-lambda?style=for-the-badge)

Simple lambda for converting office file formats into PDF files built on top of LibreOffice using https://github.com/jacobtread/libreofficekit

This repository contains two separate crates, the first being `office-convert-lambda` which is the crate for the lambda itself. The second is `office-convert-lambda-client` in the client directory which is a library crate providing a client for interacting with the lambda.

## Lambda Container Image

This lambda is provided as a container image to use on AWS lambda, you can source the Docker image from one of the following repositories:

### AWS ECR (Recommended)

```
public.ecr.aws/jacobtread/office-convert-lambda:0.1.0
```

### GHCR

```
ghcr.io/jacobtread/office-convert-lambda:0.1.0
```

### Docker Hub

```
jacobtread/office-convert-lambda:0.1.0
```
