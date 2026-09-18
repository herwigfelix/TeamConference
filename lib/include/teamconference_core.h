/*
 * TeamConference Core — C-API (lib/src/ffi.rs).
 *
 * Ein Singleton; alle Zeichenketten UTF-8, nullterminiert. Rückgabe 1 = ok,
 * 0 = nein/fehlgeschlagen, sofern nichts anderes steht. Alle Funktionen von
 * EINEM Aufrufer-Thread benutzen; die Bibliothek arbeitet intern mit eigener
 * Tokio-Runtime und eigenen Audio-Threads.
 *
 * Diese Datei von Hand mit ffi.rs synchron halten.
 */
#ifndef TEAMCONFERENCE_CORE_H
#define TEAMCONFERENCE_CORE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Lebenszyklus */
int         tc_create(void);
void        tc_destroy(void);
const char* tc_version(void);
const char* tc_core_version(void);
const char* tc_last_error(void);                 /* "" wenn keiner */

#ifdef __ANDROID__
/* Nur Android, VOR dem ersten Raumbeitritt: JavaVM* und eine globale Referenz
 * (NewGlobalRef) auf den Anwendungs-Context. cpal/oboe brauchen das für JNI. */
int         tc_android_init(void* java_vm, void* context);
#endif

/* Verbindung — asynchron; Ergebnis als Ereignis (auth_response /
 * connect_failed / connection_lost). login_json = data-Objekt von auth_login. */
int         tc_connect(const char* host, int port, int udp_port, int ssl, const char* login_json);
void        tc_disconnect(void);
int         tc_is_connected(void);
int         tc_is_authenticated(void);
int64_t     tc_user_id(void);                    /* 0 wenn nicht angemeldet */

/* Steuerkanal. tc_poll_event: Länge (ohne NUL); 0 = nichts;
 * negativ = -benötigte Länge (Ereignis bleibt in der Schlange). */
int         tc_send(const char* json);
int         tc_poll_event(char* buf, int cap);

/* Räume. password darf NULL sein. */
int         tc_join_room(int64_t room_id, const char* password);
void        tc_leave_room(void);
int64_t     tc_current_room(void);               /* 0 = keiner */

/* Mikrofon / Ton */
void        tc_set_mute(int muted);
int         tc_get_mute(void);
void        tc_set_deafen(int deafened);
int         tc_get_deafen(void);
void        tc_set_volume(float gain);
void        tc_set_user_volume(int64_t user_id, float gain);
int         tc_set_input_device(const char* name_or_null);
int         tc_list_input_devices(char* buf, int cap);   /* JSON ["name",...] */

/* Dateistream */
int         tc_stream_file(const char* path);
void        tc_stream_stop(void);
void        tc_stream_pause(int paused);
int         tc_stream_is_paused(void);
void        tc_stream_seek(int delta_seconds);
void        tc_stream_set_volume(float gain);
int         tc_stream_is_active(void);

/* Audio-Ausgabe: i16 LE, interleaved, 48000 Hz; der Aufrufer spielt ab. */
void        tc_audio_format(int* sample_rate, int* channels);
int         tc_read_audio(unsigned char* buf, int cap);  /* Bytes; 0 = nichts */
void        tc_clear_audio(void);

/* Dateien im Raum */
int         tc_upload_file(int64_t room_id, const char* path);
int         tc_download_file(int64_t file_id, const char* dest_path);

#ifdef __cplusplus
}
#endif

#endif /* TEAMCONFERENCE_CORE_H */
