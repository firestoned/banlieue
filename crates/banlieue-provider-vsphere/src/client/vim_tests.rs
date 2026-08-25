// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the BYOC HTTP-client helpers in `vim.rs` (ADR-0008).
//!
//! These cover the pure, side-effect-free pieces — PEM parsing and reqwest
//! client construction — without a vCenter. The `ClientBuilder::build().await`
//! path needs a live endpoint and is exercised by integration tests / vcsim.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::*;

    // ----------------------------------------------------------------------
    // server_address: Provider.spec.connection.endpoint (a full URL) -> the
    // bare host[:port] vim_rs 0.5 wants (it builds `https://{server_address}/
    // api/...`). Regression: passing the full URL yielded
    // `https://https://vcenter/sdk/api/...` and every connect failed.
    // ----------------------------------------------------------------------

    #[test]
    fn server_address_strips_scheme_and_sdk_path() {
        assert_eq!(
            server_address("https://vcenter.example.com/sdk"),
            "vcenter.example.com"
        );
    }

    #[test]
    fn server_address_keeps_explicit_port() {
        assert_eq!(
            server_address("https://vcenter.example.com:8443/sdk"),
            "vcenter.example.com:8443"
        );
    }

    #[test]
    fn server_address_passes_through_bare_host() {
        assert_eq!(server_address("vcenter.example.com"), "vcenter.example.com");
    }

    #[test]
    fn server_address_handles_scheme_only_and_trailing_slash() {
        assert_eq!(
            server_address("https://vcenter.example.com"),
            "vcenter.example.com"
        );
        assert_eq!(
            server_address("https://vcenter.example.com/"),
            "vcenter.example.com"
        );
    }

    #[test]
    fn server_address_keeps_ipv6_literal() {
        assert_eq!(server_address("https://[2001:db8::1]/sdk"), "[2001:db8::1]");
    }

    // Two distinct self-signed test CAs (CN=banlieue-test-ca-a / -b), generated
    // with `openssl req -x509 -newkey rsa:2048 -nodes`. Used to verify a bundle
    // with multiple concatenated certs is fully parsed (from_pem_bundle, not
    // from_pem which takes only the first).
    const TEST_CA_A: &str = "-----BEGIN CERTIFICATE-----
MIIDGzCCAgOgAwIBAgIUJn/SQVpN4u/L3trC79FdyWOFKFEwDQYJKoZIhvcNAQEL
BQAwHTEbMBkGA1UEAwwSYmFubGlldWUtdGVzdC1jYS1hMB4XDTI2MDYwMjAwMzIz
MFoXDTM2MDUzMDAwMzIzMFowHTEbMBkGA1UEAwwSYmFubGlldWUtdGVzdC1jYS1h
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA1jNVEvlefenDRXIyyh4D
ZNNSvwQ3ZV9cZoPEqovHtjokkR+7fZdy9KzvbF+gvUdar0MBlREcLqB1NokAdY+6
+PpP94ij/2Hl58Iqrri5Dg2uBvESfY1lNoNVWmSODl93OmSKIvdHzEkYkOgLKBik
5tLV9LVcOUzBJd+BoIElk1fixg3qaiYg/L+nyg8R8c8KZzQxRonGzELy91lNsf8u
eKMCmBU7b+VOATvjG2r/ECyd1OxV4yklHgxV5zZmboytlLSp+pE8Iu/EWdKh1dD+
05kDSya3BwFuPhHXBM6Vo55a2bcCJOuDgfG78NsvaHSYpaW0Q9GX7Il37N2HiRrh
rwIDAQABo1MwUTAdBgNVHQ4EFgQUIsElFZH/WqwrRVnRF2HkX8c4kPcwHwYDVR0j
BBgwFoAUIsElFZH/WqwrRVnRF2HkX8c4kPcwDwYDVR0TAQH/BAUwAwEB/zANBgkq
hkiG9w0BAQsFAAOCAQEAmCC1c9t5jLUsdh2bSU/4M5owV0Fxpl4HnInwaHQQSIsD
Q38qbBtMnG5YoYptuff3QFx+d/juIKyHlPovdDwD0OYJU5UvMznOUpaCnDPofXNl
dybiqj7uF8BIlS41kyApMKPimH87twDjd9DjfzmzUaL2HeDbq3qeFi8EcWmsD+gn
8WYdiuy0yF9z5rfbQRz1DUnkXtaQEMR8avcOAQ4Jpf+nox6egSF5OhMg2HKznQKw
C0xw7FWQWSEAH+LcwRwo/8l0gqgJf7tZDvfbbZUa7Y48f5UxUNvlmgcHYoPKYoRr
n17Lktsw0jAZJp1tU1DJPZSYHZPPWLZlJhHftNtpKQ==
-----END CERTIFICATE-----
";

    #[test]
    fn root_certs_from_pem_parses_single_cert() {
        let certs = root_certs_from_pem(TEST_CA_A).expect("valid PEM");
        assert_eq!(certs.len(), 1);
    }

    #[test]
    fn root_certs_from_pem_parses_multi_cert_bundle() {
        // A bundle of the same cert twice is still two PEM blocks: proves we
        // read every block, not just the first.
        let bundle = format!("{TEST_CA_A}{TEST_CA_A}");
        let certs = root_certs_from_pem(&bundle).expect("valid bundle");
        assert_eq!(certs.len(), 2);
    }

    #[test]
    fn root_certs_from_pem_rejects_garbage() {
        // Non-PEM input parses to zero certs; we must fail closed rather than
        // silently fall back to system roots.
        let err = root_certs_from_pem("not a pem").unwrap_err();
        assert!(err.to_string().contains("caBundle"), "got: {err}");
        assert!(err.to_string().contains("no certificates"), "got: {err}");
    }

    #[test]
    fn root_certs_from_pem_rejects_empty_string() {
        let err = root_certs_from_pem("").unwrap_err();
        assert!(err.to_string().contains("no certificates"), "got: {err}");
    }

    // Building a reqwest 0.13 client (rustls-no-provider) requires the process
    // crypto provider to be installed, exactly as production does at startup.
    // `install_default_crypto_provider` is idempotent, so every test that builds
    // a client calls it first.
    fn ensure_provider() {
        install_default_crypto_provider();
    }

    #[test]
    fn install_default_crypto_provider_is_idempotent() {
        // Calling twice must not panic (second install is a no-op).
        install_default_crypto_provider();
        install_default_crypto_provider();
    }

    #[test]
    fn build_http_client_succeeds_with_no_bundle() {
        ensure_provider();
        // None bundle, secure: uses system roots, must still build.
        assert!(build_http_client(None, false).is_ok());
    }

    #[test]
    fn build_http_client_succeeds_with_ca_bundle() {
        ensure_provider();
        assert!(build_http_client(Some(TEST_CA_A), false).is_ok());
    }

    #[test]
    fn build_http_client_succeeds_insecure() {
        ensure_provider();
        assert!(build_http_client(None, true).is_ok());
    }

    #[test]
    fn build_http_client_fails_on_invalid_pem() {
        let err = build_http_client(Some("garbage"), false).unwrap_err();
        assert!(err.to_string().contains("caBundle"), "got: {err}");
    }

    /// SEC-012: a vCenter that accepts the connection but never replies must
    /// fail the request, not hang the reconcile forever. The production
    /// deadlines are too slow for a test, so this goes through
    /// `build_http_client_with_timeouts` — the same builder path — with
    /// millisecond deadlines against a listener that holds the socket open
    /// and answers nothing.
    #[tokio::test]
    async fn request_times_out_against_a_hung_endpoint() {
        ensure_provider();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming() {
                held.push(stream); // accepted, never answered
            }
        });

        let client = build_http_client_with_timeouts(
            None,
            false,
            Duration::from_millis(200),
            Duration::from_millis(200),
        )
        .expect("client builds");
        let err = client
            .get(format!("http://{addr}/sdk"))
            .send()
            .await
            .unwrap_err();
        assert!(err.is_timeout(), "expected a timeout, got {err:?}");
    }

    // ----------------------------------------------------------------------
    // install_poll_max_attempts / find_cdrom_key — pure ADR-0021 helpers for
    // the install-then-generalize sequence, unit-testable without a live VM.
    // ----------------------------------------------------------------------

    #[test]
    fn install_poll_max_attempts_uses_the_given_timeout() {
        // 1800s / 5s interval = 360 exact polls.
        assert_eq!(install_poll_max_attempts(1800), 360);
    }

    #[test]
    fn install_poll_max_attempts_rounds_up_a_non_multiple() {
        // 12s / 5s interval = 2.4 -> ceil to 3, never fewer polls than the
        // requested window allows.
        assert_eq!(install_poll_max_attempts(12), 3);
    }

    #[test]
    fn install_poll_max_attempts_falls_back_when_unset() {
        assert_eq!(
            install_poll_max_attempts(0),
            install_poll_max_attempts(i32::try_from(DEFAULT_INSTALL_TIMEOUT_SECS).unwrap())
        );
    }

    #[test]
    fn install_poll_max_attempts_falls_back_when_negative() {
        assert_eq!(
            install_poll_max_attempts(-1),
            install_poll_max_attempts(i32::try_from(DEFAULT_INSTALL_TIMEOUT_SECS).unwrap())
        );
    }

    fn cdrom_device(key: i32) -> Box<dyn vim_rs::types::traits::VirtualDeviceTrait> {
        Box::new(VirtualCdrom {
            virtual_device_: VirtualDevice {
                key,
                ..Default::default()
            },
        })
    }

    fn disk_device(key: i32) -> Box<dyn vim_rs::types::traits::VirtualDeviceTrait> {
        Box::new(VirtualDisk {
            virtual_device_: VirtualDevice {
                key,
                ..Default::default()
            },
            ..Default::default()
        })
    }

    #[test]
    fn find_cdrom_key_locates_the_cdrom_among_other_devices() {
        let devices = vec![disk_device(2000), cdrom_device(3000)];
        assert_eq!(find_cdrom_key(&devices), Some(3000));
    }

    #[test]
    fn find_cdrom_key_none_when_no_cdrom_present() {
        let devices = vec![disk_device(2000)];
        assert_eq!(find_cdrom_key(&devices), None);
    }

    #[test]
    fn find_cdrom_key_empty_device_list_is_none() {
        let devices: Vec<Box<dyn vim_rs::types::traits::VirtualDeviceTrait>> = vec![];
        assert_eq!(find_cdrom_key(&devices), None);
    }

    // ----------------------------------------------------------------------
    // build_template_config_spec.boot_options — the created VM must always
    // boot the ISO before falling back to the (blank, then installed) disk.
    // Regression: EFI firmware only got a `boot_options` block when secure
    // boot was requested, so a plain `efi` template's firmware had no boot
    // order at all and stopped at the interactive UEFI Boot Manager menu
    // instead of auto-booting the installer (found via live testing of
    // ADR-0021's power-on step, which is the first time this VM is ever
    // actually started).
    // ----------------------------------------------------------------------

    fn minimal_iso_import_request(firmware: Firmware) -> crate::client::IsoImportRequest {
        crate::client::IsoImportRequest {
            datacenter: "dc1".to_string(),
            datacenter_moref: "datacenter-1".to_string(),
            cluster: "cluster1".to_string(),
            cluster_moref: "domain-c1".to_string(),
            datastore: "ds1".to_string(),
            nics: vec![crate::client::RequestedNic {
                network: "vmnet-prod".to_string(),
                network_moref: "network-1".to_string(),
                network_distributed: false,
                adapter: NicAdapter::Vmxnet3,
                pci_slot: 192,
            }],
            disk_gib: 100,
            disk_provisioning: DiskProvisioning::Thin,
            disk_controller: DiskController::Pvscsi,
            cpus: 2,
            memory_mib: 4096,
            firmware,
            folder: None,
            iso_datastore_path: "[ds1] banlieue-images/kairos.iso".to_string(),
            template_name: "kairos-rhel98".to_string(),
            guest_id: "rhel9_64Guest".to_string(),
            force_create: false,
            install_timeout_seconds: 1800,
            auto_manage_install: true,
        }
    }

    fn dummy_nic_backing() -> Box<dyn VirtualDeviceBackingInfoTrait> {
        Box::new(VirtualEthernetCardNetworkBackingInfo {
            virtual_device_device_backing_info_: VirtualDeviceDeviceBackingInfo {
                device_name: "vmnet-prod".to_string(),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    // build_template_config_spec deliberately sets NO boot order (matches
    // create-kairos-template.sh) — only the firmware-level secure-boot flag.
    // Boot order is a separate, post-create reconfigure: see
    // build_boot_order_reconfigure_spec below.

    #[test]
    fn build_template_config_spec_omits_boot_options_for_plain_efi() {
        let req = minimal_iso_import_request(Firmware::Efi);
        let spec = build_template_config_spec(&req, vec![dummy_nic_backing()]);
        assert!(spec.boot_options.is_none());
    }

    #[test]
    fn build_template_config_spec_omits_boot_options_for_bios() {
        let req = minimal_iso_import_request(Firmware::Bios);
        let spec = build_template_config_spec(&req, vec![dummy_nic_backing()]);
        assert!(spec.boot_options.is_none());
    }

    #[test]
    fn build_template_config_spec_sets_secure_boot_for_efi_secure() {
        let req = minimal_iso_import_request(Firmware::EfiSecure);
        let spec = build_template_config_spec(&req, vec![dummy_nic_backing()]);
        assert_eq!(
            spec.boot_options.as_ref().unwrap().efi_secure_boot_enabled,
            Some(true)
        );
        assert!(spec.boot_options.as_ref().unwrap().boot_order.is_none());
    }

    // ----------------------------------------------------------------------
    // find_disk_key / build_boot_order_reconfigure_spec
    // (ADR-0021, found live: boot order must be set post-create, by real
    // device key, mirroring create-vm.sh's govc device.connect + device.boot)
    // ----------------------------------------------------------------------

    fn ethernet_device(
        adapter: NicAdapter,
        key: i32,
        slot: Option<i32>,
    ) -> Box<dyn vim_rs::types::traits::VirtualDeviceTrait> {
        let base = VirtualEthernetCard {
            virtual_device_: VirtualDevice {
                key,
                slot_info: slot.map(|pci_slot_number| {
                    Box::new(VirtualDevicePciBusSlotInfo { pci_slot_number })
                        as Box<dyn vim_rs::types::traits::VirtualDeviceBusSlotInfoTrait>
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        match adapter {
            NicAdapter::Vmxnet3 => Box::new(VirtualVmxnet3 {
                virtual_vmxnet_: VirtualVmxnet {
                    virtual_ethernet_card_: base,
                },
                ..Default::default()
            }),
            NicAdapter::Vmxnet2 => Box::new(VirtualVmxnet2 {
                virtual_vmxnet_: VirtualVmxnet {
                    virtual_ethernet_card_: base,
                },
            }),
            NicAdapter::E1000 => Box::new(VirtualE1000 {
                virtual_ethernet_card_: base,
            }),
            NicAdapter::E1000e => Box::new(VirtualE1000E {
                virtual_ethernet_card_: base,
            }),
        }
    }

    #[test]
    fn find_disk_key_locates_the_disk_among_other_devices() {
        let devices = vec![cdrom_device(3000), disk_device(2000)];
        assert_eq!(find_disk_key(&devices), Some(2000));
    }

    #[test]
    fn find_disk_key_none_when_no_disk_present() {
        let devices = vec![cdrom_device(3000)];
        assert_eq!(find_disk_key(&devices), None);
    }

    // ----------------------------------------------------------------------
    // find_first_nic_key / build_nic_edit_device (ADR-0024's clone_vm)
    // ----------------------------------------------------------------------

    #[test]
    fn find_first_nic_key_matches_any_adapter_type() {
        let devices = vec![
            disk_device(2000),
            ethernet_device(NicAdapter::Vmxnet3, 4000, None),
        ];
        assert_eq!(
            find_first_nic_key(&devices),
            Some((
                4000,
                vim_rs::types::struct_enum::StructType::VirtualVmxnet3,
                None
            ))
        );
    }

    #[test]
    fn find_first_nic_key_none_when_no_nic_present() {
        let devices = vec![disk_device(2000)];
        assert_eq!(find_first_nic_key(&devices), None);
    }

    #[test]
    fn find_first_nic_key_also_reports_the_devices_pinned_pci_slot() {
        // Found live: a clone lost its template's ens192 naming because the
        // clone's NIC edit never re-pinned the PCI slot, so vCenter
        // auto-assigned a fresh one (ens33) instead of keeping the
        // template's. This is the read half of that fix — the slot must be
        // readable off the template's own device before it can be
        // reapplied by `build_nic_edit_device`.
        let devices = vec![ethernet_device(NicAdapter::Vmxnet3, 4000, Some(192))];
        assert_eq!(
            find_first_nic_key(&devices),
            Some((
                4000,
                vim_rs::types::struct_enum::StructType::VirtualVmxnet3,
                Some(192)
            ))
        );
    }

    #[test]
    fn build_nic_edit_device_produces_the_same_concrete_type_for_every_known_adapter() {
        // vCenter rejects a device spec naming the abstract VirtualEthernetCard
        // base type directly (found live: InvalidDeviceSpec) — every adapter
        // type find_first_nic_key can report must round-trip to that exact
        // concrete type, never the abstract base.
        use vim_rs::types::struct_enum::StructType;
        for adapter_type in [
            StructType::VirtualVmxnet3,
            StructType::VirtualVmxnet2,
            StructType::VirtualVmxnet,
            StructType::VirtualE1000,
            StructType::VirtualE1000E,
        ] {
            let device = build_nic_edit_device(4000, dummy_nic_backing(), adapter_type, None);
            assert_eq!(device.data_type(), adapter_type);
            assert_eq!(device.get_virtual_device().key, 4000);
            assert!(device.get_virtual_device().slot_info.is_none());
        }
    }

    #[test]
    fn build_nic_edit_device_pins_the_given_pci_slot_when_the_template_had_one() {
        // The write half of the ens192 fix: reproduce the template's exact
        // PCI slot on the clone's NIC rather than leaving vCenter to
        // auto-assign a fresh one (which yields a different, unpredictable
        // `ensNN` interface name inside the guest).
        use vim_rs::types::struct_enum::StructType;
        let device = build_nic_edit_device(
            4000,
            dummy_nic_backing(),
            StructType::VirtualVmxnet3,
            Some(192),
        );
        let slot = device
            .get_virtual_device()
            .slot_info
            .as_deref()
            .expect("slot_info must be set when a pci_slot is given");
        assert_eq!(slot.data_type(), StructType::VirtualDevicePciBusSlotInfo);
        let pci = slot
            .as_any_ref()
            .downcast_ref::<VirtualDevicePciBusSlotInfo>()
            .expect("slot_info must be a VirtualDevicePciBusSlotInfo");
        assert_eq!(pci.pci_slot_number, 192);
    }

    #[test]
    fn build_nic_edit_device_omits_slot_info_when_the_template_had_none() {
        // A template built before this fix (or by any means outside
        // banlieue's own create_vm path) may report no slot_info at all —
        // must not fabricate one.
        use vim_rs::types::struct_enum::StructType;
        let device =
            build_nic_edit_device(4000, dummy_nic_backing(), StructType::VirtualVmxnet3, None);
        assert!(device.get_virtual_device().slot_info.is_none());
    }

    #[test]
    fn build_template_config_spec_leaves_the_nic_slot_and_extra_config_unset() {
        // Neither the structured slot_info (found live: silently reassigned
        // by vCenter) nor an inline ethernetN.pciSlotNumber ExtraConfig
        // entry (found live: also didn't stick, even with nothing else
        // touching the device afterward) may be set in the CreateVM_Task
        // itself — see
        // build_nic_pci_slot_extra_config_reconfigure_spec_sets_ethernetn_pci_slot_number
        // below for the mechanism that actually works: a wholly separate
        // post-create ReconfigVM_Task.
        let req = minimal_iso_import_request(Firmware::Efi);
        let spec = build_template_config_spec(&req, vec![dummy_nic_backing()]);
        let nic_change = spec
            .device_change
            .as_ref()
            .unwrap()
            .iter()
            .find(|d| {
                d.device.data_type() == vim_rs::types::struct_enum::StructType::VirtualVmxnet3
            })
            .expect("NIC device present in CreateVM spec");
        assert!(
            nic_change.device.get_virtual_device().slot_info.is_none(),
            "CreateVM_Task must not request a slot for the NIC alongside auto-assigned siblings"
        );
        assert!(
            spec.extra_config.is_none(),
            "CreateVM_Task must not set ethernetN.pciSlotNumber inline either"
        );
    }

    #[test]
    fn build_nic_pci_slot_extra_config_reconfigure_spec_sets_ethernetn_pci_slot_number() {
        // The actual mechanism `create-kairos-template.sh` (this project's
        // own reference) uses for a stable ens192: a bare `govc vm.create`
        // followed by a wholly separate `govc vm.change -e
        // "ethernet0.pciSlotNumber=192"` once the VM already exists. This
        // spec is that second step — no device_change, ExtraConfig only.
        let spec = build_nic_pci_slot_extra_config_reconfigure_spec(&[192, 193]);
        assert!(
            spec.device_change.is_none(),
            "this reconfigure must touch only extraConfig, never device_change"
        );
        let extra_config = spec
            .extra_config
            .as_ref()
            .expect("extra_config must be set");
        for (i, want) in [(0, "192"), (1, "193")] {
            let entry = extra_config
                .iter()
                .find(|ov| ov.get_option_value().key == format!("ethernet{i}.pciSlotNumber"))
                .unwrap_or_else(|| panic!("ethernet{i}.pciSlotNumber must be present"));
            let value = entry
                .get_option_value()
                .value
                .as_ref()
                .unwrap_or_else(|| panic!("ethernet{i}.pciSlotNumber must have a value"));
            match value {
                vim_rs::types::vim_any::VimAny::Value(
                    vim_rs::types::boxed_types::ValueElements::PrimitiveString(s),
                ) => assert_eq!(s, want),
                other => panic!("expected a PrimitiveString value, got {other:?}"),
            }
        }
    }

    #[test]
    fn build_boot_order_reconfigure_spec_connects_cdrom_and_orders_cdrom_disk_ethernet() {
        let cdrom = CdromPlacement {
            key: 3000,
            controller_key: Some(200),
            unit_number: Some(0),
        };
        let spec = build_boot_order_reconfigure_spec(
            cdrom,
            "[ds1] banlieue-images/kairos.iso",
            2000,
            4000,
        );

        let boot_order = spec
            .boot_options
            .as_ref()
            .unwrap()
            .boot_order
            .as_ref()
            .unwrap();
        assert_eq!(
            boot_order.iter().map(|d| d.data_type()).collect::<Vec<_>>(),
            vec![
                vim_rs::types::struct_enum::StructType::VirtualMachineBootOptionsBootableCdromDevice,
                vim_rs::types::struct_enum::StructType::VirtualMachineBootOptionsBootableDiskDevice,
                vim_rs::types::struct_enum::StructType::VirtualMachineBootOptionsBootableEthernetDevice,
            ]
        );

        let device_change = spec.device_change.as_ref().unwrap();
        assert_eq!(device_change.len(), 1);
        assert_eq!(
            device_change[0].operation,
            Some(VirtualDeviceConfigSpecOperationEnum::Edit)
        );
        let edited = device_change[0].device.get_virtual_device();
        assert_eq!(edited.key, 3000);
        // Regression: an Edit device_change that omits controllerKey fails
        // live with vCenter's MissingController fault, even though only
        // `connectable` is actually changing.
        assert_eq!(edited.controller_key, Some(200));
        assert_eq!(edited.unit_number, Some(0));
        assert!(edited.backing.is_some());
        let connectable = edited.connectable.as_ref().unwrap();
        assert!(connectable.connected);
        assert!(connectable.start_connected);
    }

    #[test]
    fn map_vim_power_state_covers_every_known_variant() {
        assert_eq!(
            map_vim_power_state(&VirtualMachinePowerStateEnum::PoweredOn),
            Some(PowerState::PoweredOn)
        );
        assert_eq!(
            map_vim_power_state(&VirtualMachinePowerStateEnum::PoweredOff),
            Some(PowerState::PoweredOff)
        );
        assert_eq!(
            map_vim_power_state(&VirtualMachinePowerStateEnum::Suspended),
            Some(PowerState::Suspended)
        );
    }

    #[test]
    fn map_vim_power_state_returns_none_for_the_unknown_catch_all() {
        assert_eq!(
            map_vim_power_state(&VirtualMachinePowerStateEnum::Other_(
                "futureValue".to_string()
            )),
            None
        );
    }
}
