# TeamConference Protocol Documentation

## Overview

TeamConference uses two channels:
- **Control Channel**: WebSocket over TCP (optionally TLS) for commands, chat, auth, file transfer
- **Audio Channel**: UDP for raw PCM audio data

## Control Channel (WebSocket, JSON)

### Message Format

```json
{
  "type": "message_type",
  "id": "optional-request-id",
  "data": { ... }
}
```

### Authentication

| Type | Direction | Data |
|------|-----------|------|
| `auth_login` | C→S | `{ username, password, nickname }` |
| `auth_response` | S→C | `{ success, user_id, token, server_name, rooms, error? }` |

### Rooms

| Type | Direction | Data |
|------|-----------|------|
| `room_join` | C→S | `{ room_id, password? }` |
| `room_leave` | C→S | `{ room_id }` |
| `room_list` | S→C | `{ rooms: [{ id, name, parent_id, users, ... }] }` |
| `room_user_joined` | S→C | `{ room_id, user: { id, nickname, ... } }` |
| `room_user_left` | S→C | `{ room_id, user_id }` |
| `room_create` | C→S | `{ name, parent_id?, password?, max_users? }` (Admin) |
| `room_delete` | C→S | `{ room_id }` (Admin) |
| `room_update` | C→S | `{ room_id, name?, password?, max_users? }` (Admin) |

### Chat

| Type | Direction | Data |
|------|-----------|------|
| `chat_room` | C→S / S→C | `{ room_id, message, from_user? }` |
| `chat_private` | C→S / S→C | `{ to_user_id, message, from_user? }` |
| `chat_server` | S→C | `{ message }` |

### Audio Configuration

| Type | Direction | Data |
|------|-----------|------|
| `audio_config` | C→S | `{ sample_rate, bit_depth, channels, enabled }` |
| `audio_config_ack` | S→C | `{ success, udp_token }` |
| `audio_mute` | C→S | `{ muted: bool }` |
| `audio_deafen` | C→S | `{ deafened: bool }` |
| `audio_user_state` | S→C | `{ user_id, muted, deafened }` |

### Files

| Type | Direction | Data |
|------|-----------|------|
| `file_upload_start` | C→S | `{ room_id, filename, size }` |
| `file_upload_ack` | S→C | `{ upload_id, success }` |
| `file_upload_chunk` | C→S | `{ upload_id, data (base64), offset }` |
| `file_upload_complete` | C→S | `{ upload_id }` |
| `file_list` | C→S / S→C | `{ room_id, files: [...] }` |
| `file_download` | C→S | `{ file_id }` |
| `file_download_data` | S→C | `{ file_id, data (base64), offset, total }` |

### Admin

| Type | Direction | Data |
|------|-----------|------|
| `admin_kick` | C→S | `{ user_id, reason? }` |
| `admin_ban` | C→S | `{ user_id, reason?, duration_minutes? }` |
| `admin_move` | C→S | `{ user_id, room_id }` |
| `admin_mute` | C→S | `{ user_id, muted: bool }` |
| `admin_server_message` | C→S | `{ message }` |
| `user_kicked` | S→C | `{ reason? }` |
| `user_banned` | S→C | `{ reason?, expires_at? }` |
| `user_moved` | S→C | `{ room_id, room_name }` |

### Audio File Streaming

| Type | Direction | Data |
|------|-----------|------|
| `stream_file_start` | C→S | `{ filename, room_id }` |
| `stream_file_stop` | C→S | `{}` |
| `stream_file_status` | S→C | `{ user_id, filename, playing }` |

## Audio Channel (UDP, Binary)

### Packet Format

```
Offset  Size    Field
0       4       Magic: 0x54434F4E ("TCON")
4       4       Session Token (uint32, LE)
8       4       Sequence Number (uint32, LE)
12      4       Timestamp (uint32, milliseconds, LE)
16      2       Sample Rate (uint16, LE)
18      1       Bit Depth (uint8)
19      1       Channels (uint8, 1=Mono, 2=Stereo)
20      2       Payload Length (uint16, LE)
22      N       PCM Audio Data (Raw Samples, Little-Endian)
```

Header size: 22 bytes. All multi-byte fields are little-endian.

### Audio Data

- Raw PCM samples in little-endian byte order
- 16-bit: signed int16
- 24-bit: packed as int32 (lower 24 bits)
- 32-bit: signed int32
- Typical packet: 20ms at 48kHz mono 16-bit = 1920 bytes payload
