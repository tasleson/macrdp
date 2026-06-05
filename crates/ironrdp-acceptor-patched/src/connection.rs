use core::mem;

use ironrdp_connector::{
    encode_x224_packet, general_err, reason_err, ConnectorError, ConnectorErrorExt as _,
    ConnectorResult, DesktopSize, Sequence, State, Written,
};
use ironrdp_core::{decode, WriteBuf};
use ironrdp_pdu as pdu;
use ironrdp_pdu::nego::SecurityProtocol;
use ironrdp_pdu::x224::X224;
use ironrdp_svc::{StaticChannelSet, SvcServerProcessor};
use pdu::rdp::capability_sets::CapabilitySet;
use pdu::rdp::client_info::Credentials;
use pdu::rdp::headers::ShareControlPdu;
use pdu::rdp::server_error_info::{ErrorInfo, ProtocolIndependentCode, ServerSetErrorInfoPdu};
use pdu::rdp::server_license::{LicensePdu, LicensingErrorMessage};
use pdu::{gcc, mcs, nego, rdp};
use tracing::{debug, warn};

use super::channel_connection::ChannelConnectionSequence;
use super::finalization::FinalizationSequence;
use crate::util::{self, wrap_share_data};

const IO_CHANNEL_ID: u16 = 1003;
const USER_CHANNEL_ID: u16 = 1002;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialCheck {
    Accepted,
    NeedsClientPrompt,
    Rejected,
}

fn check_credentials(
    server_creds: Option<&Credentials>,
    client_creds: &Credentials,
) -> CredentialCheck {
    let Some(server_creds) = server_creds else {
        return CredentialCheck::Rejected;
    };

    if server_creds.username == client_creds.username
        && server_creds.password == client_creds.password
    {
        return CredentialCheck::Accepted;
    }

    if client_creds.username.is_empty() && client_creds.password.is_empty() {
        return CredentialCheck::NeedsClientPrompt;
    }

    CredentialCheck::Rejected
}

/// Pick the security protocol to confirm to the client.
///
/// NLA/CredSSP (HYBRID_EX preferred, then HYBRID) is the default path for
/// standard RDP clients; we only fall back to TLS-only (SSL) when the client
/// did not advertise any HYBRID variant. Empty-on-both-sides yields the
/// no-security legacy path. Anything else means no common protocol — return
/// `None` so the caller can fail the connection.
pub(crate) fn select_security_protocol(
    requested: SecurityProtocol,
    supported: SecurityProtocol,
) -> Option<SecurityProtocol> {
    let common = requested & supported;
    if common.intersects(SecurityProtocol::HYBRID_EX) {
        Some(SecurityProtocol::HYBRID_EX)
    } else if common.intersects(SecurityProtocol::HYBRID) {
        Some(SecurityProtocol::HYBRID)
    } else if common.intersects(SecurityProtocol::SSL) {
        Some(SecurityProtocol::SSL)
    } else if supported.is_empty() {
        Some(SecurityProtocol::empty())
    } else {
        None
    }
}

pub struct Acceptor {
    pub(crate) state: AcceptorState,
    security: SecurityProtocol,
    io_channel_id: u16,
    user_channel_id: u16,
    desktop_size: DesktopSize,
    requested_desktop_size: Option<DesktopSize>,
    server_capabilities: Vec<CapabilitySet>,
    static_channels: StaticChannelSet,
    saved_for_reactivation: AcceptorState,
    pub(crate) creds: Option<Credentials>,
    reactivation: bool,
}

#[derive(Debug)]
pub struct AcceptorResult {
    pub static_channels: StaticChannelSet,
    pub capabilities: Vec<CapabilitySet>,
    pub input_events: Vec<Vec<u8>>,
    pub user_channel_id: u16,
    pub io_channel_id: u16,
    pub reactivation: bool,
}

impl Acceptor {
    pub fn new(
        security: SecurityProtocol,
        desktop_size: DesktopSize,
        capabilities: Vec<CapabilitySet>,
        creds: Option<Credentials>,
    ) -> Self {
        Self {
            security,
            state: AcceptorState::InitiationWaitRequest,
            user_channel_id: USER_CHANNEL_ID,
            io_channel_id: IO_CHANNEL_ID,
            desktop_size,
            requested_desktop_size: None,
            server_capabilities: capabilities,
            static_channels: StaticChannelSet::new(),
            saved_for_reactivation: Default::default(),
            creds,
            reactivation: false,
        }
    }

    pub fn new_deactivation_reactivation(
        mut consumed: Acceptor,
        static_channels: StaticChannelSet,
        desktop_size: DesktopSize,
    ) -> ConnectorResult<Self> {
        let AcceptorState::CapabilitiesSendServer {
            early_capability,
            channels,
        } = consumed.saved_for_reactivation
        else {
            return Err(general_err!("invalid acceptor state"));
        };

        for cap in consumed.server_capabilities.iter_mut() {
            if let CapabilitySet::Bitmap(cap) = cap {
                cap.desktop_width = desktop_size.width;
                cap.desktop_height = desktop_size.height;
            }
        }
        let state = AcceptorState::CapabilitiesSendServer {
            early_capability,
            channels: channels.clone(),
        };
        let saved_for_reactivation = AcceptorState::CapabilitiesSendServer {
            early_capability,
            channels,
        };
        Ok(Self {
            security: consumed.security,
            state,
            user_channel_id: consumed.user_channel_id,
            io_channel_id: consumed.io_channel_id,
            desktop_size,
            requested_desktop_size: None,
            server_capabilities: consumed.server_capabilities,
            static_channels,
            saved_for_reactivation,
            creds: consumed.creds,
            reactivation: true,
        })
    }

    pub fn attach_static_channel<T>(&mut self, channel: T)
    where
        T: SvcServerProcessor + 'static,
    {
        self.static_channels.insert(channel);
    }

    pub fn take_requested_desktop_size(&mut self) -> Option<DesktopSize> {
        self.requested_desktop_size.take()
    }

    pub fn set_desktop_size(&mut self, desktop_size: DesktopSize) {
        self.desktop_size = desktop_size;
        for cap in self.server_capabilities.iter_mut() {
            if let CapabilitySet::Bitmap(cap) = cap {
                cap.desktop_width = desktop_size.width;
                cap.desktop_height = desktop_size.height;
            }
        }
    }

    pub fn reached_security_upgrade(&self) -> Option<SecurityProtocol> {
        match self.state {
            AcceptorState::SecurityUpgrade { .. } => Some(self.security),
            _ => None,
        }
    }

    /// # Panics
    ///
    /// Panics if state is not [AcceptorState::SecurityUpgrade].
    pub fn mark_security_upgrade_as_done(&mut self) {
        assert!(self.reached_security_upgrade().is_some());
        self.step(&[], &mut WriteBuf::new())
            .expect("transition to next state");
        debug_assert!(self.reached_security_upgrade().is_none());
    }

    pub fn should_perform_credssp(&self) -> bool {
        matches!(self.state, AcceptorState::Credssp { .. })
    }

    /// # Panics
    ///
    /// Panics if state is not [AcceptorState::Credssp].
    pub fn mark_credssp_as_done(&mut self) {
        assert!(self.should_perform_credssp());
        let res = self
            .step(&[], &mut WriteBuf::new())
            .expect("transition to next state");
        debug_assert!(!self.should_perform_credssp());
        assert_eq!(res, Written::Nothing);
    }

    pub fn get_result(&mut self) -> Option<AcceptorResult> {
        match mem::take(&mut self.state) {
            AcceptorState::Accepted {
                channels: _channels, // TODO: what about ChannelDef?
                client_capabilities,
                input_events,
            } => Some(AcceptorResult {
                static_channels: mem::take(&mut self.static_channels),
                capabilities: client_capabilities,
                input_events,
                user_channel_id: self.user_channel_id,
                io_channel_id: self.io_channel_id,
                reactivation: self.reactivation,
            }),
            previous_state => {
                self.state = previous_state;
                None
            }
        }
    }
}

#[derive(Default, Debug)]
pub enum AcceptorState {
    #[default]
    Consumed,

    InitiationWaitRequest,
    InitiationSendConfirm {
        requested_protocol: SecurityProtocol,
    },
    SecurityUpgrade {
        requested_protocol: SecurityProtocol,
        protocol: SecurityProtocol,
    },
    Credssp {
        requested_protocol: SecurityProtocol,
        protocol: SecurityProtocol,
    },
    BasicSettingsWaitInitial {
        requested_protocol: SecurityProtocol,
        protocol: SecurityProtocol,
    },
    BasicSettingsSendResponse {
        requested_protocol: SecurityProtocol,
        protocol: SecurityProtocol,
        early_capability: Option<gcc::ClientEarlyCapabilityFlags>,
        channels: Vec<(u16, Option<gcc::ChannelDef>)>,
    },
    ChannelConnection {
        protocol: SecurityProtocol,
        early_capability: Option<gcc::ClientEarlyCapabilityFlags>,
        channels: Vec<(u16, gcc::ChannelDef)>,
        connection: ChannelConnectionSequence,
    },
    RdpSecurityCommencement {
        protocol: SecurityProtocol,
        early_capability: Option<gcc::ClientEarlyCapabilityFlags>,
        channels: Vec<(u16, gcc::ChannelDef)>,
    },
    SecureSettingsExchange {
        protocol: SecurityProtocol,
        early_capability: Option<gcc::ClientEarlyCapabilityFlags>,
        channels: Vec<(u16, gcc::ChannelDef)>,
    },
    LicensingExchange {
        early_capability: Option<gcc::ClientEarlyCapabilityFlags>,
        channels: Vec<(u16, gcc::ChannelDef)>,
    },
    CapabilitiesSendServer {
        early_capability: Option<gcc::ClientEarlyCapabilityFlags>,
        channels: Vec<(u16, gcc::ChannelDef)>,
    },
    MonitorLayoutSend {
        channels: Vec<(u16, gcc::ChannelDef)>,
    },
    CapabilitiesWaitConfirm {
        channels: Vec<(u16, gcc::ChannelDef)>,
    },
    ConnectionFinalization {
        finalization: FinalizationSequence,
        channels: Vec<(u16, gcc::ChannelDef)>,
        client_capabilities: Vec<CapabilitySet>,
    },
    Accepted {
        channels: Vec<(u16, gcc::ChannelDef)>,
        client_capabilities: Vec<CapabilitySet>,
        input_events: Vec<Vec<u8>>,
    },
}

impl State for AcceptorState {
    fn name(&self) -> &'static str {
        match self {
            Self::Consumed => "Consumed",
            Self::InitiationWaitRequest => "InitiationWaitRequest",
            Self::InitiationSendConfirm { .. } => "InitiationSendConfirm",
            Self::SecurityUpgrade { .. } => "SecurityUpgrade",
            Self::Credssp { .. } => "Credssp",
            Self::BasicSettingsWaitInitial { .. } => "BasicSettingsWaitInitial",
            Self::BasicSettingsSendResponse { .. } => "BasicSettingsSendResponse",
            Self::ChannelConnection { .. } => "ChannelConnection",
            Self::RdpSecurityCommencement { .. } => "RdpSecurityCommencement",
            Self::SecureSettingsExchange { .. } => "SecureSettingsExchange",
            Self::LicensingExchange { .. } => "LicensingExchange",
            Self::CapabilitiesSendServer { .. } => "CapabilitiesSendServer",
            Self::MonitorLayoutSend { .. } => "MonitorLayoutSend",
            Self::CapabilitiesWaitConfirm { .. } => "CapabilitiesWaitConfirm",
            Self::ConnectionFinalization { .. } => "ConnectionFinalization",
            Self::Accepted { .. } => "Connected",
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(self, Self::Accepted { .. })
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

impl Sequence for Acceptor {
    fn next_pdu_hint(&self) -> Option<&dyn pdu::PduHint> {
        match &self.state {
            AcceptorState::Consumed => None,
            AcceptorState::InitiationWaitRequest => Some(&pdu::X224_HINT),
            AcceptorState::InitiationSendConfirm { .. } => None,
            AcceptorState::SecurityUpgrade { .. } => None,
            AcceptorState::Credssp { .. } => None,
            AcceptorState::BasicSettingsWaitInitial { .. } => Some(&pdu::X224_HINT),
            AcceptorState::BasicSettingsSendResponse { .. } => None,
            AcceptorState::ChannelConnection { connection, .. } => connection.next_pdu_hint(),
            AcceptorState::RdpSecurityCommencement { .. } => None,
            AcceptorState::SecureSettingsExchange { .. } => Some(&pdu::X224_HINT),
            AcceptorState::LicensingExchange { .. } => None,
            AcceptorState::CapabilitiesSendServer { .. } => None,
            AcceptorState::MonitorLayoutSend { .. } => None,
            AcceptorState::CapabilitiesWaitConfirm { .. } => Some(&pdu::X224_HINT),
            AcceptorState::ConnectionFinalization { finalization, .. } => {
                finalization.next_pdu_hint()
            }
            AcceptorState::Accepted { .. } => None,
        }
    }

    fn state(&self) -> &dyn State {
        &self.state
    }

    fn step(&mut self, input: &[u8], output: &mut WriteBuf) -> ConnectorResult<Written> {
        let prev_state = mem::take(&mut self.state);

        let (written, next_state) = match prev_state {
            AcceptorState::InitiationWaitRequest => {
                let connection_request = decode::<X224<nego::ConnectionRequest>>(input)
                    .map_err(ConnectorError::decode)
                    .map(|p| p.0)?;

                debug!(message = ?connection_request, "Received");

                (
                    Written::Nothing,
                    AcceptorState::InitiationSendConfirm {
                        requested_protocol: connection_request.protocol,
                    },
                )
            }

            AcceptorState::InitiationSendConfirm { requested_protocol } => {
                let protocol = select_security_protocol(requested_protocol, self.security)
                    .ok_or_else(|| {
                        ConnectorError::general("failed to negotiate security protocol")
                    })?;
                let connection_confirm = nego::ConnectionConfirm::Response {
                    flags: nego::ResponseFlags::empty(),
                    protocol,
                };

                debug!(message = ?connection_confirm, "Send");

                let written = ironrdp_core::encode_buf(&X224(connection_confirm), output)
                    .map_err(ConnectorError::encode)?;

                (
                    Written::from_size(written)?,
                    AcceptorState::SecurityUpgrade {
                        requested_protocol,
                        protocol,
                    },
                )
            }

            AcceptorState::SecurityUpgrade {
                requested_protocol,
                protocol,
            } => {
                debug!(?requested_protocol);
                let next_state = if protocol
                    .intersects(SecurityProtocol::HYBRID | SecurityProtocol::HYBRID_EX)
                {
                    AcceptorState::Credssp {
                        requested_protocol,
                        protocol,
                    }
                } else {
                    AcceptorState::BasicSettingsWaitInitial {
                        requested_protocol,
                        protocol,
                    }
                };
                (Written::Nothing, next_state)
            }

            AcceptorState::Credssp {
                requested_protocol,
                protocol,
            } => (
                Written::Nothing,
                AcceptorState::BasicSettingsWaitInitial {
                    requested_protocol,
                    protocol,
                },
            ),

            AcceptorState::BasicSettingsWaitInitial {
                requested_protocol,
                protocol,
            } => {
                let x224_payload = decode::<X224<pdu::x224::X224Data<'_>>>(input)
                    .map_err(ConnectorError::decode)
                    .map(|p| p.0)?;
                let settings_initial = decode::<mcs::ConnectInitial>(x224_payload.data.as_ref())
                    .map_err(ConnectorError::decode)?;

                debug!(message = ?settings_initial, "Received");

                let gcc_blocks = settings_initial.conference_create_request.into_gcc_blocks();
                let early_capability = gcc_blocks.core.optional_data.early_capability_flags;
                self.requested_desktop_size = Some(DesktopSize {
                    width: gcc_blocks.core.desktop_width,
                    height: gcc_blocks.core.desktop_height,
                });

                let joined: Vec<_> = gcc_blocks
                    .network
                    .map(|network| {
                        network
                            .channels
                            .into_iter()
                            .map(|c| {
                                self.static_channels
                                    .get_by_channel_name(&c.name)
                                    .map(|(type_id, _)| (type_id, c))
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                #[expect(clippy::arithmetic_side_effects)]
                // IO channel ID is not big enough for overflowing.
                let channels = joined
                    .into_iter()
                    .enumerate()
                    .map(|(i, channel)| {
                        let channel_id =
                            u16::try_from(i).expect("always in the range") + self.io_channel_id + 1;
                        if let Some((type_id, c)) = channel {
                            self.static_channels.attach_channel_id(type_id, channel_id);
                            (channel_id, Some(c))
                        } else {
                            (channel_id, None)
                        }
                    })
                    .collect();

                (
                    Written::Nothing,
                    AcceptorState::BasicSettingsSendResponse {
                        requested_protocol,
                        protocol,
                        early_capability,
                        channels,
                    },
                )
            }

            AcceptorState::BasicSettingsSendResponse {
                requested_protocol,
                protocol,
                early_capability,
                channels,
            } => {
                let channel_ids: Vec<u16> = channels.iter().map(|&(i, _)| i).collect();

                let skip_channel_join = early_capability.is_some_and(|client| {
                    client.contains(gcc::ClientEarlyCapabilityFlags::SUPPORT_SKIP_CHANNELJOIN)
                });

                let server_blocks = create_gcc_blocks(
                    self.io_channel_id,
                    channel_ids.clone(),
                    requested_protocol,
                    skip_channel_join,
                );

                let settings_response = mcs::ConnectResponse {
                    conference_create_response: gcc::ConferenceCreateResponse::new(
                        self.user_channel_id,
                        server_blocks,
                    )
                    .map_err(ConnectorError::decode)?,
                    called_connect_id: 1,
                    domain_parameters: mcs::DomainParameters::target(),
                };

                debug!(message = ?settings_response, "Send");

                let written = encode_x224_packet(&settings_response, output)?;
                let channels = channels
                    .into_iter()
                    .filter_map(|(i, c)| c.map(|c| (i, c)))
                    .collect();

                (
                    Written::from_size(written)?,
                    AcceptorState::ChannelConnection {
                        protocol,
                        early_capability,
                        channels,
                        connection: if skip_channel_join {
                            ChannelConnectionSequence::skip_channel_join(self.user_channel_id)
                        } else {
                            ChannelConnectionSequence::new(
                                self.user_channel_id,
                                self.io_channel_id,
                                channel_ids,
                            )
                        },
                    },
                )
            }

            AcceptorState::ChannelConnection {
                protocol,
                early_capability,
                channels,
                mut connection,
            } => {
                let written = connection.step(input, output)?;
                let state = if connection.is_done() {
                    AcceptorState::RdpSecurityCommencement {
                        protocol,
                        early_capability,
                        channels,
                    }
                } else {
                    AcceptorState::ChannelConnection {
                        protocol,
                        early_capability,
                        channels,
                        connection,
                    }
                };

                (written, state)
            }

            AcceptorState::RdpSecurityCommencement {
                protocol,
                early_capability,
                channels,
                ..
            } => (
                Written::Nothing,
                AcceptorState::SecureSettingsExchange {
                    protocol,
                    early_capability,
                    channels,
                },
            ),

            AcceptorState::SecureSettingsExchange {
                protocol,
                early_capability,
                channels,
            } => {
                let data: X224<mcs::SendDataRequest<'_>> =
                    decode(input).map_err(ConnectorError::decode)?;
                let data = data.0;
                let client_info: rdp::ClientInfoPdu =
                    decode(data.user_data.as_ref()).map_err(ConnectorError::decode)?;

                debug!(message = ?client_info, "Received");

                if !protocol.intersects(SecurityProtocol::HYBRID | SecurityProtocol::HYBRID_EX) {
                    let creds = client_info.client_info.credentials;

                    warn!(
                        client_user = %creds.username,
                        client_domain = ?creds.domain,
                        server_user = ?self.creds.as_ref().map(|c| &c.username),
                        protocol = ?protocol,
                        "Credential check (non-hybrid)"
                    );

                    let credential_check = check_credentials(self.creds.as_ref(), &creds);

                    if credential_check == CredentialCheck::Rejected {
                        warn!(
                            client_user = %creds.username,
                            client_domain = ?creds.domain,
                            server_user = ?self.creds.as_ref().map(|c| &c.username),
                            "Credential mismatch \u{2014} rejecting"
                        );
                        let info = ServerSetErrorInfoPdu(ErrorInfo::ProtocolIndependentCode(
                            ProtocolIndependentCode::ServerDeniedConnection,
                        ));

                        debug!(message = ?info, "Send");

                        util::encode_send_data_indication(
                            self.user_channel_id,
                            self.io_channel_id,
                            &info,
                            output,
                        )?;

                        return Err(ConnectorError::general("invalid credentials"));
                    }
                    if credential_check == CredentialCheck::NeedsClientPrompt {
                        debug!(user = %creds.username, domain = ?creds.domain, "Empty credentials \u{2014} client will prompt user");
                    } else {
                        debug!(user = %creds.username, domain = ?creds.domain, "Credentials accepted");
                    }
                }

                (
                    Written::Nothing,
                    AcceptorState::LicensingExchange {
                        early_capability,
                        channels,
                    },
                )
            }

            AcceptorState::LicensingExchange {
                early_capability,
                channels,
            } => {
                let license: LicensePdu = LicensingErrorMessage::new_valid_client()
                    .map_err(ConnectorError::encode)?
                    .into();

                debug!(message = ?license, "Send");

                let written = util::encode_send_data_indication(
                    self.user_channel_id,
                    self.io_channel_id,
                    &license,
                    output,
                )?;

                self.saved_for_reactivation = AcceptorState::CapabilitiesSendServer {
                    early_capability,
                    channels: channels.clone(),
                };

                (
                    Written::from_size(written)?,
                    AcceptorState::CapabilitiesSendServer {
                        early_capability,
                        channels,
                    },
                )
            }

            AcceptorState::CapabilitiesSendServer {
                early_capability,
                channels,
            } => {
                let demand_active = rdp::headers::ShareControlHeader {
                    share_id: 0,
                    pdu_source: self.io_channel_id,
                    share_control_pdu: ShareControlPdu::ServerDemandActive(
                        rdp::capability_sets::ServerDemandActive {
                            pdu: rdp::capability_sets::DemandActive {
                                source_descriptor: "".into(),
                                capability_sets: self.server_capabilities.clone(),
                            },
                        },
                    ),
                };

                debug!(message = ?demand_active, "Send");

                let written = util::encode_send_data_indication(
                    self.user_channel_id,
                    self.io_channel_id,
                    &demand_active,
                    output,
                )?;

                let layout_flag = gcc::ClientEarlyCapabilityFlags::SUPPORT_MONITOR_LAYOUT_PDU;
                let next_state = if early_capability.is_some_and(|c| c.contains(layout_flag)) {
                    AcceptorState::MonitorLayoutSend { channels }
                } else {
                    AcceptorState::CapabilitiesWaitConfirm { channels }
                };

                (Written::from_size(written)?, next_state)
            }

            AcceptorState::MonitorLayoutSend { channels } => {
                let monitor_layout = rdp::headers::ShareDataPdu::MonitorLayout(
                    rdp::finalization_messages::MonitorLayoutPdu {
                        monitors: vec![gcc::Monitor {
                            left: 0,
                            top: 0,
                            right: i32::from(self.desktop_size.width),
                            bottom: i32::from(self.desktop_size.height),
                            flags: gcc::MonitorFlags::PRIMARY,
                        }],
                    },
                );

                debug!(message = ?monitor_layout, "Send");

                let share_data = wrap_share_data(monitor_layout, self.io_channel_id);

                let written = util::encode_send_data_indication(
                    self.user_channel_id,
                    self.io_channel_id,
                    &share_data,
                    output,
                )?;

                (
                    Written::from_size(written)?,
                    AcceptorState::CapabilitiesWaitConfirm { channels },
                )
            }

            AcceptorState::CapabilitiesWaitConfirm { ref channels } => {
                let message = decode::<X224<mcs::McsMessage<'_>>>(input)
                    .map_err(ConnectorError::decode)
                    .map(|p| p.0);
                let message = match message {
                    Ok(msg) => msg,
                    Err(e) => {
                        if self.reactivation {
                            debug!("Dropping unexpected PDU during reactivation");
                            self.state = prev_state;
                            return Ok(Written::Nothing);
                        } else {
                            return Err(e);
                        }
                    }
                };
                match message {
                    mcs::McsMessage::SendDataRequest(data) => {
                        let capabilities_confirm =
                            decode::<rdp::headers::ShareControlHeader>(data.user_data.as_ref())
                                .map_err(ConnectorError::decode);
                        let capabilities_confirm = match capabilities_confirm {
                            Ok(capabilities_confirm) => capabilities_confirm,
                            Err(e) => {
                                if self.reactivation {
                                    debug!("Dropping unexpected PDU during reactivation");
                                    self.state = prev_state;
                                    return Ok(Written::Nothing);
                                } else {
                                    return Err(e);
                                }
                            }
                        };

                        debug!(message = ?capabilities_confirm, "Received");

                        let ShareControlPdu::ClientConfirmActive(confirm) =
                            capabilities_confirm.share_control_pdu
                        else {
                            return Err(ConnectorError::general("expected client confirm active"));
                        };

                        (
                            Written::Nothing,
                            AcceptorState::ConnectionFinalization {
                                channels: channels.clone(),
                                finalization: FinalizationSequence::new(
                                    self.user_channel_id,
                                    self.io_channel_id,
                                ),
                                client_capabilities: confirm.pdu.capability_sets,
                            },
                        )
                    }

                    mcs::McsMessage::DisconnectProviderUltimatum(ultimatum) => {
                        return Err(reason_err!(
                            "received disconnect ultimatum",
                            "{:?}",
                            ultimatum.reason
                        ))
                    }

                    _ => {
                        warn!(?message, "Unexpected MCS message received");

                        (Written::Nothing, prev_state)
                    }
                }
            }

            AcceptorState::ConnectionFinalization {
                mut finalization,
                channels,
                client_capabilities,
            } => {
                let written = finalization.step(input, output)?;

                let state = if finalization.is_done() {
                    AcceptorState::Accepted {
                        channels,
                        client_capabilities,
                        input_events: finalization.into_input_events(),
                    }
                } else {
                    AcceptorState::ConnectionFinalization {
                        finalization,
                        channels,
                        client_capabilities,
                    }
                };

                (written, state)
            }

            _ => unreachable!(),
        };

        self.state = next_state;
        Ok(written)
    }
}

fn create_gcc_blocks(
    io_channel: u16,
    channel_ids: Vec<u16>,
    requested: SecurityProtocol,
    skip_channel_join: bool,
) -> gcc::ServerGccBlocks {
    gcc::ServerGccBlocks {
        core: gcc::ServerCoreData {
            version: gcc::RdpVersion::V5_PLUS,
            optional_data: gcc::ServerCoreOptionalData {
                client_requested_protocols: Some(requested),
                early_capability_flags: skip_channel_join
                    .then_some(gcc::ServerEarlyCapabilityFlags::SKIP_CHANNELJOIN_SUPPORTED),
            },
        },
        security: gcc::ServerSecurityData::no_security(),
        network: gcc::ServerNetworkData {
            channel_ids,
            io_channel,
        },
        message_channel: None,
        multi_transport_channel: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{check_credentials, select_security_protocol, CredentialCheck};
    use ironrdp_pdu::nego::SecurityProtocol;
    use ironrdp_pdu::rdp::client_info::Credentials;

    const SERVER_NLA: SecurityProtocol = SecurityProtocol::from_bits_truncate(
        SecurityProtocol::HYBRID.bits()
            | SecurityProtocol::HYBRID_EX.bits()
            | SecurityProtocol::SSL.bits(),
    );

    fn credentials(username: &str, password: &str) -> Credentials {
        Credentials {
            username: String::from(username),
            password: String::from(password),
            domain: None,
        }
    }

    #[test]
    fn accepts_matching_username_and_password() {
        let server = credentials("macrdp", "secret");
        let client = credentials("macrdp", "secret");

        assert_eq!(
            check_credentials(Some(&server), &client),
            CredentialCheck::Accepted
        );
    }

    #[test]
    fn rejects_empty_password_for_configured_non_empty_password() {
        let server = credentials("macrdp", "secret");
        let client = credentials("macrdp", "");

        assert_eq!(
            check_credentials(Some(&server), &client),
            CredentialCheck::Rejected
        );
    }

    #[test]
    fn treats_fully_blank_credentials_as_prompt_required_not_accepted() {
        let server = credentials("macrdp", "secret");
        let client = credentials("", "");

        assert_eq!(
            check_credentials(Some(&server), &client),
            CredentialCheck::NeedsClientPrompt
        );
    }

    #[test]
    fn accepts_empty_password_only_when_it_matches_server_credentials() {
        let server = credentials("macrdp", "");
        let client = credentials("macrdp", "");

        assert_eq!(
            check_credentials(Some(&server), &client),
            CredentialCheck::Accepted
        );
    }

    #[test]
    fn rejects_when_server_credentials_are_missing() {
        let client = credentials("macrdp", "secret");

        assert_eq!(check_credentials(None, &client), CredentialCheck::Rejected);
    }

    #[test]
    fn rejects_when_only_username_matches() {
        let server = credentials("macrdp", "secret");
        let client = credentials("macrdp", "wrong");

        assert_eq!(
            check_credentials(Some(&server), &client),
            CredentialCheck::Rejected
        );
    }

    #[test]
    fn rejects_when_only_password_matches() {
        let server = credentials("macrdp", "secret");
        let client = credentials("other", "secret");

        assert_eq!(
            check_credentials(Some(&server), &client),
            CredentialCheck::Rejected
        );
    }

    #[test]
    fn comparison_is_case_sensitive_on_both_fields() {
        let server = credentials("macrdp", "Secret");

        // mstsc preserves the case the user typed; we don't fold it.
        assert_eq!(
            check_credentials(Some(&server), &credentials("MACRDP", "Secret")),
            CredentialCheck::Rejected
        );
        assert_eq!(
            check_credentials(Some(&server), &credentials("macrdp", "secret")),
            CredentialCheck::Rejected
        );
    }

    #[test]
    fn whitespace_only_client_credentials_are_not_treated_as_prompt() {
        // The NeedsClientPrompt path is strictly "both literally empty"; a single
        // space is a real (non-matching) credential and must be rejected, not
        // silently treated as an unattempted login.
        let server = credentials("macrdp", "secret");
        let client = credentials(" ", " ");

        assert_eq!(
            check_credentials(Some(&server), &client),
            CredentialCheck::Rejected
        );
    }

    #[test]
    fn domain_field_is_ignored_during_credential_match() {
        let server = Credentials {
            username: "macrdp".to_owned(),
            password: "secret".to_owned(),
            domain: Some("CORP".to_owned()),
        };
        let client_no_domain = credentials("macrdp", "secret");
        let client_other_domain = Credentials {
            username: "macrdp".to_owned(),
            password: "secret".to_owned(),
            domain: Some("OTHER".to_owned()),
        };

        assert_eq!(
            check_credentials(Some(&server), &client_no_domain),
            CredentialCheck::Accepted
        );
        assert_eq!(
            check_credentials(Some(&server), &client_other_domain),
            CredentialCheck::Accepted
        );
    }

    #[test]
    fn selects_hybrid_ex_when_client_supports_full_nla() {
        let client = SecurityProtocol::HYBRID | SecurityProtocol::HYBRID_EX | SecurityProtocol::SSL;
        assert_eq!(
            select_security_protocol(client, SERVER_NLA),
            Some(SecurityProtocol::HYBRID_EX)
        );
    }

    #[test]
    fn selects_hybrid_when_client_lacks_hybrid_ex() {
        let client = SecurityProtocol::HYBRID | SecurityProtocol::SSL;
        assert_eq!(
            select_security_protocol(client, SERVER_NLA),
            Some(SecurityProtocol::HYBRID)
        );
    }

    #[test]
    fn falls_back_to_ssl_only_when_client_has_no_nla() {
        let client = SecurityProtocol::SSL;
        assert_eq!(
            select_security_protocol(client, SERVER_NLA),
            Some(SecurityProtocol::SSL)
        );
    }

    #[test]
    fn fails_when_client_and_server_share_no_protocol() {
        let client = SecurityProtocol::empty();
        assert_eq!(select_security_protocol(client, SERVER_NLA), None);
    }

    #[test]
    fn nla_first_even_when_client_lists_ssl_first() {
        // Order of bits in the client's requested protocol must not influence
        // selection; only the strongest mutually supported protocol wins.
        let client = SecurityProtocol::SSL | SecurityProtocol::HYBRID;
        assert_eq!(
            select_security_protocol(client, SERVER_NLA),
            Some(SecurityProtocol::HYBRID)
        );
    }
}
