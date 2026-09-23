// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The libvirt operations the `LibvirtMachine` reconciler needs (ADR-0050).
//!
//! # Why this is not on [`LibvirtClient`](crate::client::LibvirtClient)
//!
//! The Provider reconciler only *reads*, and reads the same two lists every
//! time, so `client.rs` fetches them once and hands back a
//! `SnapshotClient` holding no connection at all. A machine reconcile
//! mutates: it creates volumes, defines a domain, starts it, and later tears
//! all of that down. Those must happen on one live session, in order, and
//! several of them take `&mut` because a session carries one in-flight call
//! at a time. Bolting them onto a snapshot-shaped trait would mean either
//! lying about the connection's lifetime or reconnecting per call.
//!
//! # "Not found" is a normal answer
//!
//! libvirt reports a missing object as an error reply, so every lookup here
//! returns `Result<Option<_>>` and maps the three not-found codes to `None`
//! via `banlieue_libvirt::is_not_found`. A reconciler asking "does this
//! exist yet?" is the most common question it asks, and it must not have to
//! pattern-match error codes to get an answer.

use async_trait::async_trait;
use banlieue_api::banlieue::ProviderConnection;
use banlieue_libvirt::{
    DEVICE_MODIFY_EJECT, Domain, DomainInterface, DomainState, InterfaceAddressSource, Session,
    StoragePool, StorageVol, TlsIdentity, connect_open, connect_tls, domain_create,
    domain_define_xml, domain_destroy, domain_get_state, domain_interface_addresses,
    domain_lookup_by_name, domain_undefine, domain_update_device_flags, is_not_found,
    storage_pool_lookup_by_name, storage_pool_refresh, storage_vol_create_xml, storage_vol_delete,
    storage_vol_lookup_by_name, storage_vol_upload,
};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::client::{LOCAL_DRIVER_URI, parse_endpoint};
use crate::error::{Error, Result};

/// The order [`LibvirtMachineClient::domain_addresses`] tries its sources in.
///
/// Authority descending, which is not the same as convenience descending: the
/// agent reports what the guest actually configured, a lease reports only
/// what DHCP offered (which the guest may have ignored, or which may be
/// stale), and ARP is an inference from traffic that has already happened.
/// Reporting a lease address as the VM's address when the guest took a
/// different one sends every consumer to the wrong host.
pub const ADDRESS_SOURCE_ORDER: [InterfaceAddressSource; 3] = [
    InterfaceAddressSource::Agent,
    InterfaceAddressSource::Lease,
    InterfaceAddressSource::Arp,
];

/// Mutating libvirt operations, on one live session.
#[async_trait]
pub trait LibvirtMachineClient: Send {
    /// Look up a storage pool by name. `None` when it does not exist.
    ///
    /// # Errors
    /// [`Error::Libvirt`] on anything but a not-found reply.
    async fn lookup_pool(&mut self, name: &str) -> Result<Option<StoragePool>>;

    /// Look up a volume in `pool`. `None` when it does not exist.
    ///
    /// The returned [`StorageVol::key`] is the volume's path on the host for
    /// a directory pool, which is what the domain XML needs.
    ///
    /// # Errors
    /// [`Error::Libvirt`] on anything but a not-found reply.
    async fn lookup_volume(&mut self, pool: &StoragePool, name: &str)
    -> Result<Option<StorageVol>>;

    /// Create a volume in `pool` from `xml`.
    ///
    /// # Errors
    /// [`Error::Libvirt`] if creation fails.
    async fn create_volume(&mut self, pool: &StoragePool, xml: &str) -> Result<StorageVol>;

    /// Stream `data` into an existing volume.
    ///
    /// Used for the cloud-init seed (ADR-0054), which is small enough to
    /// hold in memory — unlike a guest image, whose transfer runs in a Job
    /// precisely so gigabytes never pass through a reconcile loop
    /// (ADR-0011).
    ///
    /// # Errors
    /// [`Error::Libvirt`] if the stream fails.
    async fn upload_volume(&mut self, vol: &StorageVol, data: &[u8]) -> Result<()>;

    /// Delete a volume. Absent is success, so teardown is idempotent.
    ///
    /// # Errors
    /// [`Error::Libvirt`] on anything but a not-found reply.
    async fn delete_volume(&mut self, vol: &StorageVol) -> Result<()>;

    /// Re-scan a pool's backing directory, so a volume just written is
    /// visible to the next lookup.
    ///
    /// # Errors
    /// [`Error::Libvirt`] if the refresh fails.
    async fn refresh_pool(&mut self, pool: &StoragePool) -> Result<()>;

    /// Look up a domain by name. `None` when it does not exist.
    ///
    /// # Errors
    /// [`Error::Libvirt`] on anything but a not-found reply.
    async fn lookup_domain(&mut self, name: &str) -> Result<Option<Domain>>;

    /// Define (persist) a domain from XML without starting it. Defining over
    /// an existing domain of the same name replaces its configuration, which
    /// is what lets a reconciler converge rather than branch on existence.
    ///
    /// # Errors
    /// [`Error::Libvirt`] if libvirtd rejects the XML.
    async fn define_domain(&mut self, xml: &str) -> Result<Domain>;

    /// Start a defined domain.
    ///
    /// # Errors
    /// [`Error::Libvirt`] if the domain cannot start.
    async fn start_domain(&mut self, domain: &Domain) -> Result<Domain>;

    /// Read a domain's run state.
    ///
    /// # Errors
    /// [`Error::Libvirt`] if the state cannot be read.
    async fn domain_state(&mut self, domain: &Domain) -> Result<DomainState>;

    /// Force a domain off. Already-off is success.
    ///
    /// # Errors
    /// [`Error::Libvirt`] on anything but a not-found reply.
    async fn destroy_domain(&mut self, domain: &Domain) -> Result<()>;

    /// Undefine a domain, always removing its NVRAM and TPM state
    /// (ADR-0050 Decision 5). Absent is success.
    ///
    /// # Errors
    /// [`Error::Libvirt`] on anything but a not-found reply.
    async fn undefine_domain(&mut self, domain: &Domain) -> Result<()>;

    /// Eject the medium from the install cdrom, leaving the drive in place
    /// (ADR-0044).
    ///
    /// `xml` is the whole device element in its ejected end state; libvirt
    /// matches the existing device by its `<target dev=…>`. Applied to both
    /// the live domain and its persistent definition, so the medium cannot
    /// return at the guest's next reboot.
    ///
    /// # Errors
    /// [`Error::Libvirt`] if no device matches, or if the guest holds the
    /// tray locked — which is deliberately not forced, because a guest still
    /// reading the installer is a fact worth surfacing.
    async fn eject_install_media(&mut self, domain: &Domain, xml: &str) -> Result<()>;

    /// Read a domain's interface addresses, trying each source in
    /// [`ADDRESS_SOURCE_ORDER`] until one answers with at least one address.
    ///
    /// Returns the source that answered alongside the interfaces, so a wrong
    /// address is diagnosable. `None` when no source produced anything —
    /// the normal state while a guest is still booting, not an error.
    ///
    /// # Errors
    /// [`Error::Libvirt`] only if a source fails in a way that is not simply
    /// "this source is unavailable".
    async fn domain_addresses(
        &mut self,
        domain: &Domain,
    ) -> Result<Option<(Vec<DomainInterface>, InterfaceAddressSource)>>;

    /// Whether the domain's *installed* guest has announced itself
    /// (ADR-0043).
    ///
    /// Infallible on purpose. No agent, no marker, an unreadable one and a
    /// guest still installing are indistinguishable from "not yet", and a
    /// `Deferred` member spends most of its life legitimately in that
    /// state — so a provider must not treat any of them as an error and
    /// back off.
    async fn probe_guest(&mut self, domain: &Domain) -> crate::guest::GuestProbe;
}

/// Diagnostic helper: a domain's addresses as a printable string,
/// whichever source answers, or `None` when none does.
///
/// Exists for live tests, where "the guest is Running but has no address"
/// and "the guest never booted" look identical from the outside.
impl<S> SessionMachineClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    /// Best-effort address summary for `domain_name`.
    pub async fn domain_addresses_any(&mut self, domain_name: &str) -> Option<String> {
        let domain = self.lookup_domain(domain_name).await.ok().flatten()?;
        let (ifaces, source) = self.domain_addresses(&domain).await.ok().flatten()?;
        let listed: Vec<String> = ifaces
            .iter()
            .flat_map(|i| i.addrs.iter().map(|a| a.addr.clone()))
            .collect();
        Some(format!("{listed:?} via {source:?}"))
    }
}

/// Builds a [`LibvirtMachineClient`] against a Provider's host.
#[async_trait]
pub trait LibvirtMachineClientFactory: Send + Sync {
    /// Connect, authenticate and open a session.
    ///
    /// # Errors
    /// [`Error::Libvirt`] on transport, TLS or protocol failure.
    async fn build(
        &self,
        connection: &ProviderConnection,
        identity: &TlsIdentity,
    ) -> Result<Box<dyn LibvirtMachineClient>>;
}

/// The production factory: mutual TLS, then `CONNECT_OPEN` read-write.
#[derive(Debug, Default, Clone)]
pub struct TlsMachineClientFactory;

impl TlsMachineClientFactory {
    /// Construct.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl LibvirtMachineClientFactory for TlsMachineClientFactory {
    async fn build(
        &self,
        connection: &ProviderConnection,
        identity: &TlsIdentity,
    ) -> Result<Box<dyn LibvirtMachineClient>> {
        let (host, port) = parse_endpoint(&connection.endpoint)?;
        let mut session = connect_tls(&host, port, identity).await?;
        // read_only = false: everything this client exists to do is a write.
        connect_open(&mut session, Some(LOCAL_DRIVER_URI), false).await?;
        Ok(Box::new(SessionMachineClient { session }))
    }
}

/// A machine client holding one live session.
pub struct SessionMachineClient<S> {
    session: Session<S>,
}

impl<S> SessionMachineClient<S> {
    /// Wrap an already-opened session. Useful for the live integration test,
    /// which opens its own connection.
    pub fn new(session: Session<S>) -> Self {
        Self { session }
    }
}

/// Map a libvirt result to `Option`, turning a not-found reply into `None`.
fn optional<T>(r: banlieue_libvirt::Result<T>) -> Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) if is_not_found(&e) => Ok(None),
        Err(e) => Err(Error::from(e)),
    }
}

/// Map a libvirt result to `()`, treating not-found as success. For teardown
/// steps, where "it is already gone" is the goal, not a failure.
fn idempotent(r: banlieue_libvirt::Result<()>) -> Result<()> {
    match r {
        Ok(()) => Ok(()),
        Err(e) if is_not_found(&e) => Ok(()),
        Err(e) => Err(Error::from(e)),
    }
}

#[async_trait]
impl<S> LibvirtMachineClient for SessionMachineClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    async fn lookup_pool(&mut self, name: &str) -> Result<Option<StoragePool>> {
        optional(storage_pool_lookup_by_name(&mut self.session, name).await)
    }

    async fn lookup_volume(
        &mut self,
        pool: &StoragePool,
        name: &str,
    ) -> Result<Option<StorageVol>> {
        optional(storage_vol_lookup_by_name(&mut self.session, pool, name).await)
    }

    async fn create_volume(&mut self, pool: &StoragePool, xml: &str) -> Result<StorageVol> {
        Ok(storage_vol_create_xml(&mut self.session, pool, xml).await?)
    }

    async fn upload_volume(&mut self, vol: &StorageVol, data: &[u8]) -> Result<()> {
        let mut reader = std::io::Cursor::new(data);
        let len = u64::try_from(data.len()).unwrap_or(u64::MAX);
        storage_vol_upload(&mut self.session, vol, &mut reader, len).await?;
        Ok(())
    }

    async fn delete_volume(&mut self, vol: &StorageVol) -> Result<()> {
        idempotent(storage_vol_delete(&mut self.session, vol).await)
    }

    async fn refresh_pool(&mut self, pool: &StoragePool) -> Result<()> {
        Ok(storage_pool_refresh(&mut self.session, pool).await?)
    }

    async fn lookup_domain(&mut self, name: &str) -> Result<Option<Domain>> {
        optional(domain_lookup_by_name(&mut self.session, name).await)
    }

    async fn define_domain(&mut self, xml: &str) -> Result<Domain> {
        Ok(domain_define_xml(&mut self.session, xml).await?)
    }

    async fn start_domain(&mut self, domain: &Domain) -> Result<Domain> {
        Ok(domain_create(&mut self.session, domain).await?)
    }

    async fn domain_state(&mut self, domain: &Domain) -> Result<DomainState> {
        Ok(domain_get_state(&mut self.session, domain).await?)
    }

    async fn destroy_domain(&mut self, domain: &Domain) -> Result<()> {
        // A domain that is already off answers with an error rather than
        // succeeding, and for teardown that is the desired state — but it is
        // *not* a not-found code, so it cannot go through `idempotent`.
        match domain_destroy(&mut self.session, domain).await {
            Ok(()) => Ok(()),
            Err(e) if is_not_found(&e) => Ok(()),
            // Anything else, including "domain is not running", is reported.
            // Never `|| true` a teardown step: a destroy that silently failed
            // is how a half-torn-down domain gets reused later.
            Err(e) => Err(Error::from(e)),
        }
    }

    async fn undefine_domain(&mut self, domain: &Domain) -> Result<()> {
        idempotent(domain_undefine(&mut self.session, domain).await)
    }

    async fn eject_install_media(&mut self, domain: &Domain, xml: &str) -> Result<()> {
        domain_update_device_flags(&mut self.session, domain, xml, DEVICE_MODIFY_EJECT)
            .await
            .map_err(Error::from)
    }

    async fn probe_guest(&mut self, domain: &Domain) -> crate::guest::GuestProbe {
        crate::guest::probe_guest(&mut self.session, domain).await
    }

    async fn domain_addresses(
        &mut self,
        domain: &Domain,
    ) -> Result<Option<(Vec<DomainInterface>, InterfaceAddressSource)>> {
        for source in ADDRESS_SOURCE_ORDER {
            // A source that cannot answer errors rather than returning an
            // empty list — no guest agent, no lease file, domain not running.
            // That is a reason to try the next source, not to fail: only a
            // reply we could not *decode* would mean something is wrong.
            match domain_interface_addresses(&mut self.session, domain, source).await {
                Ok(ifaces) if ifaces.iter().any(|i| !i.addrs.is_empty()) => {
                    return Ok(Some((ifaces, source)));
                }
                Ok(_) => continue,
                Err(e @ banlieue_libvirt::TransportError::Protocol { .. }) => {
                    // A reply we could not decode is a real bug in our
                    // reading of the protocol, not an unavailable source.
                    return Err(Error::from(e));
                }
                Err(_) => continue,
            }
        }
        Ok(None)
    }
}

/// In-memory machine client for reconciler tests.
///
/// Records the calls it receives, so a test can assert on *order* — which is
/// what most of this reconciler's correctness is: volumes before the domain,
/// destroy before undefine, and never an undefine that skipped its destroy.
#[derive(Debug, Default)]
pub struct FakeMachineClient {
    /// Pools the fake host has. A lookup for anything else returns `None`.
    pub pools: Vec<StoragePool>,
    /// Volumes the fake host has, keyed by `(pool name, volume name)`.
    pub volumes: std::collections::BTreeMap<(String, String), StorageVol>,
    /// Domains currently defined, by name.
    pub domains: std::collections::BTreeMap<String, Domain>,
    /// Domains currently running, by name.
    pub running: std::collections::BTreeSet<String>,
    /// What each address source reports. An absent source is one that cannot
    /// answer, which is how a real host behaves without a guest agent.
    pub addresses_by_source:
        std::collections::BTreeMap<InterfaceAddressSourceKey, Vec<DomainInterface>>,
    /// Bytes written by `upload_volume`, by volume name.
    pub uploaded: std::collections::BTreeMap<String, Vec<u8>>,
    /// The XML each volume was created from, by volume name.
    ///
    /// Kept because discarding it made this fake more permissive than
    /// libvirt: `create_volume` used to record only the name, so a test
    /// asserting a volume's *format* could pass while the document declared
    /// the wrong one — which is exactly how an overlay went out declaring a
    /// qcow2 backing file as raw, and booted nothing.
    pub created_volume_xml: std::collections::BTreeMap<String, String>,
    /// Every call, in order, as `"<op>:<subject>"`.
    pub calls: Vec<String>,
    /// Domains whose installed guest has announced itself (ADR-0043).
    ///
    /// A set rather than a flag so the fake can distinguish "this domain
    /// reports installed" from "every domain does" — the bug that would
    /// otherwise hide is a reconciler reading the wrong domain's marker and
    /// still passing.
    pub guest_installed: std::collections::BTreeSet<String>,
    /// Domains whose `qemu-guest-agent` answers at all. Membership of
    /// `guest_installed` implies it; listing a domain here *without* the
    /// marker is how a test spells "still installing", which is the state
    /// the fast poll interval exists for.
    pub guest_agent: std::collections::BTreeSet<String>,
    /// Domains whose install media this fake has ejected (ADR-0044). A set,
    /// for the same reason as `guest_installed`: a reconciler that ejected
    /// the wrong domain's media must not pass.
    pub ejected: std::collections::BTreeSet<String>,
    /// Every domain XML passed to `define_domain`, in order. `converge`
    /// redefines on every pass, so what the LAST define contained is what
    /// the host would actually be left with — the only way to catch an
    /// eject that a later redefine silently undoes.
    pub defined_xml: Vec<String>,
    /// When set, every call fails with this message.
    pub fail_with: Option<String>,
}

/// `InterfaceAddressSource` is not `Ord`, so the fake keys its map by the
/// wire value instead. A newtype rather than a bare `u32` so a test cannot
/// accidentally index it with something meaningless.
pub type InterfaceAddressSourceKey = u32;

/// Read one simple element's text out of a document, for the fake's parsing.
fn element(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let (_, rest) = xml.split_once(&open)?;
    let (value, _) = rest.split_once(&close)?;
    Some(value.to_string())
}

/// Canonical 8-4-4-4-12 rendering, matching the reconciler's own.
fn format_uuid_bytes(uuid: &[u8; 16]) -> String {
    let hex: String = uuid.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

impl FakeMachineClient {
    /// A domain handle with a deterministic UUID, for fixtures.
    #[must_use]
    pub fn domain(name: &str) -> Domain {
        let mut uuid = [0u8; 16];
        for (i, b) in uuid.iter_mut().enumerate() {
            *b = (i as u8).wrapping_add(name.len() as u8);
        }
        Domain {
            name: name.to_string(),
            uuid,
            id: -1,
        }
    }

    fn guard(&self) -> Result<()> {
        match &self.fail_with {
            Some(m) => Err(Error::Libvirt(m.clone())),
            None => Ok(()),
        }
    }

    fn record(&mut self, op: &str, subject: &str) {
        self.calls.push(format!("{op}:{subject}"));
    }
}

#[async_trait]
impl LibvirtMachineClient for FakeMachineClient {
    async fn probe_guest(&mut self, domain: &Domain) -> crate::guest::GuestProbe {
        self.record("probe_guest", &domain.name);
        // Infallible like the real one, `fail_with` included: on a real host
        // an unreachable agent is "not yet", not an error, and a fake that
        // errored here would let a reconciler get that wrong and still pass.
        if self.guest_installed.contains(&domain.name) {
            crate::guest::GuestProbe::Installed
        } else if self.guest_agent.contains(&domain.name) {
            crate::guest::GuestProbe::NotAnnounced
        } else {
            crate::guest::GuestProbe::AgentUnreachable
        }
    }

    async fn lookup_pool(&mut self, name: &str) -> Result<Option<StoragePool>> {
        self.guard()?;
        self.record("lookup_pool", name);
        Ok(self.pools.iter().find(|p| p.name == name).cloned())
    }

    async fn lookup_volume(
        &mut self,
        pool: &StoragePool,
        name: &str,
    ) -> Result<Option<StorageVol>> {
        self.guard()?;
        self.record("lookup_volume", name);
        Ok(self
            .volumes
            .get(&(pool.name.clone(), name.to_string()))
            .cloned())
    }

    async fn create_volume(&mut self, pool: &StoragePool, xml: &str) -> Result<StorageVol> {
        self.guard()?;
        // Pull the name out of the XML the way libvirt would, so a test that
        // builds real volume XML exercises that XML.
        let name = xml
            .split_once("<name>")
            .and_then(|(_, rest)| rest.split_once("</name>"))
            .map(|(n, _)| n.to_string())
            .ok_or_else(|| Error::Invalid {
                what: "volume XML",
                detail: "no <name> element".to_string(),
            })?;
        self.record("create_volume", &name);
        self.created_volume_xml
            .insert(name.clone(), xml.to_string());
        let vol = StorageVol {
            pool: pool.name.clone(),
            name: name.clone(),
            key: format!("/var/lib/libvirt/images/{name}"),
        };
        self.volumes.insert((pool.name.clone(), name), vol.clone());
        Ok(vol)
    }

    async fn upload_volume(&mut self, vol: &StorageVol, data: &[u8]) -> Result<()> {
        self.guard()?;
        self.record("upload_volume", &vol.name);
        self.uploaded.insert(vol.name.clone(), data.to_vec());
        Ok(())
    }

    async fn delete_volume(&mut self, vol: &StorageVol) -> Result<()> {
        self.guard()?;
        self.record("delete_volume", &vol.name);
        self.volumes.remove(&(vol.pool.clone(), vol.name.clone()));
        Ok(())
    }

    async fn refresh_pool(&mut self, pool: &StoragePool) -> Result<()> {
        self.guard()?;
        self.record("refresh_pool", &pool.name);
        Ok(())
    }

    async fn lookup_domain(&mut self, name: &str) -> Result<Option<Domain>> {
        self.guard()?;
        self.record("lookup_domain", name);
        Ok(self.domains.get(name).cloned())
    }

    async fn define_domain(&mut self, xml: &str) -> Result<Domain> {
        self.guard()?;
        let name = element(xml, "name").ok_or_else(|| Error::Invalid {
            what: "domain XML",
            detail: "no <name> element".to_string(),
        })?;
        self.record("define_domain", &name);
        self.defined_xml.push(xml.to_string());

        // Models libvirt's real behaviour, which is NOT an unconditional
        // upsert: a domain is identified by UUID, so redefining one that
        // already exists without naming its UUID is refused. An earlier
        // version of this fake simply overwrote, which let that bug reach a
        // live host — the first converge succeeded and every one after it
        // failed with "already exists with uuid …". A fake that is more
        // permissive than the real thing hides exactly the bugs it exists
        // to catch.
        if let Some(existing) = self.domains.get(&name) {
            let existing_uuid = format_uuid_bytes(&existing.uuid);
            return match element(xml, "uuid").as_deref() {
                Some(u) if u == existing_uuid => Ok(existing.clone()),
                Some(u) => Err(Error::Libvirt(format!(
                    "operation failed: domain '{name}' already exists with uuid \
                     {existing_uuid} (document said {u})"
                ))),
                None => Err(Error::Libvirt(format!(
                    "operation failed: domain '{name}' already exists with uuid \
                     {existing_uuid}"
                ))),
            };
        }

        let dom = Self::domain(&name);
        self.domains.insert(name, dom.clone());
        Ok(dom)
    }

    async fn start_domain(&mut self, domain: &Domain) -> Result<Domain> {
        self.guard()?;
        self.record("start_domain", &domain.name);
        self.running.insert(domain.name.clone());
        Ok(Domain {
            id: 1,
            ..domain.clone()
        })
    }

    async fn domain_state(&mut self, domain: &Domain) -> Result<DomainState> {
        self.guard()?;
        self.record("domain_state", &domain.name);
        Ok(if self.running.contains(&domain.name) {
            DomainState::Running
        } else {
            DomainState::ShutOff
        })
    }

    async fn destroy_domain(&mut self, domain: &Domain) -> Result<()> {
        self.guard()?;
        self.record("destroy", &domain.name);
        self.running.remove(&domain.name);
        Ok(())
    }

    async fn undefine_domain(&mut self, domain: &Domain) -> Result<()> {
        self.guard()?;
        self.record("undefine", &domain.name);
        self.domains.remove(&domain.name);
        Ok(())
    }

    async fn eject_install_media(&mut self, domain: &Domain, xml: &str) -> Result<()> {
        self.guard()?;
        self.record("eject", &domain.name);
        // A fake that is more permissive than the real thing hides bugs
        // (rules/testing.md). libvirtd matches the device to update by its
        // `<target dev=…>`, so an element without one is rejected there and
        // must be rejected here too.
        if !xml.contains("<target dev='") {
            return Err(Error::Libvirt(format!(
                "update-device XML names no target device: {xml}"
            )));
        }
        self.ejected.insert(domain.name.clone());
        Ok(())
    }

    async fn domain_addresses(
        &mut self,
        domain: &Domain,
    ) -> Result<Option<(Vec<DomainInterface>, InterfaceAddressSource)>> {
        self.guard()?;
        self.record("domain_addresses", &domain.name);
        for source in ADDRESS_SOURCE_ORDER {
            let Some(ifaces) = self.addresses_by_source.get(&(source as u32)) else {
                continue;
            };
            if ifaces.iter().any(|i| !i.addrs.is_empty()) {
                return Ok(Some((ifaces.clone(), source)));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
#[path = "machine_client_tests.rs"]
mod machine_client_tests;
