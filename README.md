# Office Convert Lambda

![License](https://img.shields.io/github/license/jacobtread/office-convert-lambda?style=for-the-badge)

Simple lambda for converting office file formats into PDF files built on top of LibreOffice using https://github.com/jacobtread/libreofficekit

This repository contains two separate crates, the first being `office-convert-lambda` which is the crate for the lambda itself. The second is `office-convert-lambda-client` in the client directory which is a library crate providing a client for interacting with the lambda.

## Instructions for deploy

- Create an Elastic Container Registry for the image
- Build image for ARM `docker buildx build --platform linux/arm64 -t office-convert-lambda .`
- Follow the "Push Instructions" from the ECR page for the container image to upload it, append `--platform linux/arm64` to the push command
- Deploy ARM based container lambda using the container image
