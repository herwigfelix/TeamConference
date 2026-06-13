use std::sync::Arc;
use tokio::net::UdpSocket;

use crate::config::Config;
use crate::user::manager::UserManager;

const MAGIC: [u8; 4] = [0x54, 0x43, 0x4F, 0x4E]; // "TCON"
const HEADER_SIZE: usize = 22;

pub struct UdpAudioServer {
    socket: Arc<UdpSocket>,
    users: Arc<UserManager>,
}

impl UdpAudioServer {
    pub async fn start(config: &Config, users: Arc<UserManager>) -> anyhow::Result<Arc<Self>> {
        let addr = format!("{}:{}", config.network.audio_host, config.network.audio_port);
        let socket = UdpSocket::bind(&addr).await?;
        tracing::info!("UDP Audio server listening on {}", addr);

        let server = Arc::new(Self {
            socket: Arc::new(socket),
            users,
        });

        let srv = server.clone();
        tokio::spawn(async move {
            srv.receive_loop().await;
        });

        Ok(server)
    }

    async fn receive_loop(&self) {
        let mut buf = [0u8; 65536];
        let mut packets_received: u64 = 0;

        loop {
            match self.socket.recv_from(&mut buf).await {
                Ok((len, addr)) => {
                    if len < HEADER_SIZE {
                        tracing::debug!("UDP recv: packet too short ({} < {}), from {}", len, HEADER_SIZE, addr);
                        continue;
                    }

                    // Validate magic
                    if buf[0..4] != MAGIC {
                        tracing::debug!("UDP recv: bad magic from {}, first 4 bytes={:?}", addr, &buf[..4]);
                        continue;
                    }

                    // Parse session token
                    let token = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);

                    // Look up user by token
                    let user = match self.users.get_user_by_token(token).await {
                        Some(u) => u,
                        None => {
                            if packets_received < 5 {
                                tracing::debug!("UDP recv: unknown token={} from {}", token, addr);
                            }
                            continue;
                        }
                    };

                    packets_received += 1;
                    if packets_received == 1 {
                        tracing::info!(
                            "UDP recv: first valid packet from {}, token={}, user_id={}, room_id={:?}, muted={}, admin_muted={}, len={}",
                            addr, token, user.user_id, user.room_id, user.muted, user.admin_muted, len
                        );
                    }

                    // Update UDP address if needed
                    if user.udp_addr != Some(addr) {
                        tracing::debug!("UDP: updating addr for user {} from {:?} to {}", user.user_id, user.udp_addr, addr);
                        self.users.set_udp_addr(user.user_id, addr).await;
                    }

                    // Don't relay if muted or admin-muted
                    if user.muted || user.admin_muted {
                        if packets_received <= 3 {
                            tracing::debug!("UDP: user {} is muted={}/admin_muted={}, skipping relay", user.user_id, user.muted, user.admin_muted);
                        }
                        continue;
                    }

                    // Get room members and relay
                    if let Some(room_id) = user.room_id {
                        let packet = buf[..len].to_vec();
                        self.relay_to_room(room_id, user.user_id, &packet).await;
                    } else if packets_received <= 3 {
                        tracing::debug!("UDP: user {} has no room_id, skipping relay", user.user_id);
                    }
                }
                Err(e) => {
                    tracing::error!("UDP receive error: {}", e);
                }
            }
        }
    }

    async fn relay_to_room(&self, room_id: i64, sender_id: i64, packet: &[u8]) {
        let members = self.users.get_users_in_room(room_id).await;

        static RELAY_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let relay_num = RELAY_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        if relay_num == 0 {
            tracing::info!(
                "UDP relay: first relay for room={}, sender={}, members={}, packet_len={}",
                room_id, sender_id, members.len(), packet.len()
            );
            for m in &members {
                tracing::info!(
                    "  member: id={}, nick={}, loopback={}, deafened={}, udp_addr={:?}",
                    m.user_id, m.nickname, m.loopback, m.deafened, m.udp_addr
                );
            }
        }

        for member in members {
            // Skip sender unless loopback is enabled
            if member.user_id == sender_id && !member.loopback {
                continue;
            }

            // Don't send to deafened users
            if member.deafened {
                continue;
            }

            // Need UDP address
            if let Some(addr) = member.udp_addr {
                match self.socket.send_to(packet, addr).await {
                    Ok(bytes) => {
                        if relay_num == 0 {
                            tracing::info!("UDP relay: sent {} bytes to {} (user {})", bytes, addr, member.user_id);
                        }
                    }
                    Err(e) => {
                        tracing::debug!("UDP relay: failed to send to {} (user {}): {}", addr, member.user_id, e);
                    }
                }
            } else if relay_num == 0 {
                tracing::info!("UDP relay: member {} has no udp_addr, skipping", member.user_id);
            }
        }
    }

    pub async fn send_audio_to_room(
        &self,
        room_id: i64,
        packet: &[u8],
    ) {
        let members = self.users.get_users_in_room(room_id).await;

        for member in members {
            if member.deafened {
                continue;
            }
            if let Some(addr) = member.udp_addr {
                let _ = self.socket.send_to(packet, addr).await;
            }
        }
    }

    pub fn socket(&self) -> Arc<UdpSocket> {
        self.socket.clone()
    }
}

pub fn build_audio_packet(
    token: u32,
    seq: u32,
    timestamp_ms: u32,
    sample_rate: u16,
    bit_depth: u8,
    channels: u8,
    pcm_data: &[u8],
) -> Vec<u8> {
    let payload_len = pcm_data.len() as u16;
    let mut packet = Vec::with_capacity(HEADER_SIZE + pcm_data.len());

    packet.extend_from_slice(&MAGIC);
    packet.extend_from_slice(&token.to_le_bytes());
    packet.extend_from_slice(&seq.to_le_bytes());
    packet.extend_from_slice(&timestamp_ms.to_le_bytes());
    packet.extend_from_slice(&sample_rate.to_le_bytes());
    packet.push(bit_depth);
    packet.push(channels);
    packet.extend_from_slice(&payload_len.to_le_bytes());
    packet.extend_from_slice(pcm_data);

    packet
}
