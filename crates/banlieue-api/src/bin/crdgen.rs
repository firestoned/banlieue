// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Emit every banlieue CRD as a multi-document YAML stream on stdout.
//!
//! Build and run with:
//!     cargo run --bin crdgen --features crdgen > deploy/crds/banlieue.yaml

use banlieue_api::banlieue::{Provider, VMClass, VMImage, VirtualMachine};
use banlieue_api::infrastructure::{VSphereMachine, VSphereMachineTemplate};
use kube::CustomResourceExt;

fn main() {
    let crds = [
        // banlieue.io/v1alpha1
        serde_yaml::to_string(&Provider::crd()).unwrap(),
        serde_yaml::to_string(&VMClass::crd()).unwrap(),
        serde_yaml::to_string(&VMImage::crd()).unwrap(),
        serde_yaml::to_string(&VirtualMachine::crd()).unwrap(),
        // infrastructure.banlieue.io/v1alpha1
        serde_yaml::to_string(&VSphereMachine::crd()).unwrap(),
        serde_yaml::to_string(&VSphereMachineTemplate::crd()).unwrap(),
    ];

    for (i, doc) in crds.iter().enumerate() {
        if i > 0 {
            println!("---");
        }
        print!("{doc}");
    }
}
