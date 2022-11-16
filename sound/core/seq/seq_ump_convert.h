// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * ALSA sequencer event conversion between UMP and legacy clients
 */
#ifndef __SEQ_UMP_CONVERT_H
#define __SEQ_UMP_CONVERT_H

#include <sound/ump.h>
#include <sound/seq_kernel.h>
#include "seq_clientmgr.h"
#include "seq_ports.h"

/* MIDI 1.0 / 2.0 Status Code */
enum {
	UMP_MSG_STATUS_PER_NOTE_RCC		= 0x00,	/* MIDI2 only */
	UMP_MSG_STATUS_PER_NOTE_ACC		= 0x10,	/* MIDI2 only */
	UMP_MSG_STATUS_RPN			= 0x20,	/* MIDI2 only */
	UMP_MSG_STATUS_NRPN			= 0x30,	/* MIDI2 only */
	UMP_MSG_STATUS_RELATIVE_RPN		= 0x40,	/* MIDI2 only */
	UMP_MSG_STATUS_RELATIVE_NRPN		= 0x50,	/* MIDI2 only */
	UMP_MSG_STATUS_PER_NOTE_PITCH_BEND	= 0x60,	/* MIDI2 only */
	UMP_MSG_STATUS_NOTE_OFF			= 0x80,
	UMP_MSG_STATUS_NOTE_ON			= 0x90,
	UMP_MSG_STATUS_POLY_PRESSURE		= 0xa0,
	UMP_MSG_STATUS_CC			= 0xb0,
	UMP_MSG_STATUS_PROGRAM			= 0xc0,
	UMP_MSG_STATUS_CHANNEL_PRESSURE		= 0xd0,
	UMP_MSG_STATUS_PITCH_BEND		= 0xe0,
	UMP_MSG_STATUS_PER_NOTE_MGMT		= 0xf0,	/* MIDI2 only */
};

/* MIDI 1.0 Channel Control */
enum {
	UMP_CC_BANK_SELECT		= 0,
	UMP_CC_MODULATION		= 1,
	UMP_CC_BREATH			= 2,
	UMP_CC_FOOT			= 4,
	UMP_CC_PORTAMENTO_TIME		= 5,
	UMP_CC_DATA			= 6,
	UMP_CC_VOLUME			= 7,
	UMP_CC_BALANCE			= 8,
	UMP_CC_PAN			= 10,
	UMP_CC_EXPRESSION		= 11,
	UMP_CC_EFFECT_CONTROL_1		= 12,
	UMP_CC_EFFECT_CONTROL_2		= 13,
	UMP_CC_GP_1			= 16,
	UMP_CC_GP_2			= 17,
	UMP_CC_GP_3			= 18,
	UMP_CC_GP_4			= 19,
	UMP_CC_BANK_SELECT_LSB		= 32,
	UMP_CC_MODULATION_LSB		= 33,
	UMP_CC_BREATH_LSB		= 34,
	UMP_CC_FOOT_LSB			= 36,
	UMP_CC_PORTAMENTO_TIME_LSB	= 37,
	UMP_CC_DATA_LSB			= 38,
	UMP_CC_VOLUME_LSB		= 39,
	UMP_CC_BALANCE_LSB		= 40,
	UMP_CC_PAN_LSB			= 42,
	UMP_CC_EXPRESSION_LSB		= 43,
	UMP_CC_EFFECT1_LSB		= 44,
	UMP_CC_EFFECT2_LSB		= 45,
	UMP_CC_GP_1_LSB			= 48,
	UMP_CC_GP_2_LSB			= 49,
	UMP_CC_GP_3_LSB			= 50,
	UMP_CC_GP_4_LSB			= 51,
	UMP_CC_SUSTAIN			= 64,
	UMP_CC_PORTAMENTO_SWITCH	= 65,
	UMP_CC_SOSTENUTO		= 66,
	UMP_CC_SOFT_PEDAL		= 67,
	UMP_CC_LEGATO			= 68,
	UMP_CC_HOLD_2			= 69,
	UMP_CC_SOUND_CONTROLLER_1	= 70,
	UMP_CC_SOUND_CONTROLLER_2	= 71,
	UMP_CC_SOUND_CONTROLLER_3	= 72,
	UMP_CC_SOUND_CONTROLLER_4	= 73,
	UMP_CC_SOUND_CONTROLLER_5	= 74,
	UMP_CC_SOUND_CONTROLLER_6	= 75,
	UMP_CC_SOUND_CONTROLLER_7	= 76,
	UMP_CC_SOUND_CONTROLLER_8	= 77,
	UMP_CC_SOUND_CONTROLLER_9	= 78,
	UMP_CC_SOUND_CONTROLLER_10	= 79,
	UMP_CC_GP_5			= 80,
	UMP_CC_GP_6			= 81,
	UMP_CC_GP_7			= 82,
	UMP_CC_GP_8			= 83,
	UMP_CC_PORTAMENTO_CONTROL	= 84,
	UMP_CC_EFFECT_1			= 91,
	UMP_CC_EFFECT_2			= 92,
	UMP_CC_EFFECT_3			= 93,
	UMP_CC_EFFECT_4			= 94,
	UMP_CC_EFFECT_5			= 95,
	UMP_CC_DATA_INC			= 96,
	UMP_CC_DATA_DEC			= 97,
	UMP_CC_NRPN_LSB			= 98,
	UMP_CC_NRPN_MSB			= 99,
	UMP_CC_RPN_LSB			= 100,
	UMP_CC_RPN_MSB			= 101,
	UMP_CC_ALL_SOUND_OFF		= 120,
	UMP_CC_RESET_ALL		= 121,
	UMP_CC_LOCAL_CONTROL		= 122,
	UMP_CC_ALL_NOTES_OFF		= 123,
	UMP_CC_OMNI_OFF			= 124,
	UMP_CC_OMNI_ON			= 125,
	UMP_CC_POLY_OFF			= 126,
	UMP_CC_POLY_ON			= 127,
};

/* MIDI 1.0 / 2.0 System Messages */
enum {
	UMP_SYSTEM_STATUS_MASK			= 0xf0,
	UMP_SYSTEM_STATUS_MIDI_TIME_CODE	= 0xf1,
	UMP_SYSTEM_STATUS_SONG_POSITION		= 0xf2,
	UMP_SYSTEM_STATUS_SONG_SELECT		= 0xf3,
	UMP_SYSTEM_STATUS_TUNE_REQUEST		= 0xf6,
	UMP_SYSTEM_STATUS_TIMING_CLOCK		= 0xf8,
	UMP_SYSTEM_STATUS_START			= 0xfa,
	UMP_SYSTEM_STATUS_CONTINUE		= 0xfb,
	UMP_SYSTEM_STATUS_STOP			= 0xfc,
	UMP_SYSTEM_STATUS_ACTIVE_SENSING	= 0xfe,
	UMP_SYSTEM_STATUS_RESET			= 0xff,
};

/* MIDI 1.0 Sysex */
enum {
	UMP_MSG_MIDI1_SYSEX_START	= 0xf0,
	UMP_MSG_MIDI1_SYSEX_END		= 0xf7,
};

/* MIDI 2.0 Program Change option bit */
enum {
	UMP_PROGRAM_CHANGE_BANK_NONE		= 0x00,
	UMP_PROGRAM_CHANGE_BANK_VALID		= 0x01,
};

/*
 * UMP Message Definitions
 */

/* MIDI 1.0 Note Off / Note On */
struct snd_ump_midi1_msg_note {
	__u8 velocity;
	__u8 note;
	__u8 status_channel;
	__u8 type_group;
} __packed;

/* MIDI 1.0 Poly Pressure */
struct snd_ump_midi1_msg_paf {
	__u8 data;
	__u8 note;
	__u8 status_channel;
	__u8 type_group;
} __packed;

/* MIDI 1.0 Control Change */
struct snd_ump_midi1_msg_cc {
	__u8 data;
	__u8 index;
	__u8 status_channel;
	__u8 type_group;
} __packed;

/* MIDI 1.0 Program Change */
struct snd_ump_midi1_msg_program {
	__u8 reserved;
	__u8 program;
	__u8 status_channel;
	__u8 type_group;
} __packed;

/* MIDI 1.0 Channel Pressure */
struct snd_ump_midi1_msg_caf {
	__u8 reserved;
	__u8 data;
	__u8 status_channel;
	__u8 type_group;
} __packed;

/* MIDI 1.0 Pitch Bend */
struct snd_ump_midi1_msg_pitchbend {
	__u8 data_msb;
	__u8 data_lsb;
	__u8 status_channel;
	__u8 type_group;
} __packed;

/* MIDI 1.0 System Message */
struct snd_ump_system_msg {
	__u8 parm2;
	__u8 parm1;
	__u8 status_channel;
	__u8 type_group;
};

/* MIDI 1.0 UMP CMV */
union snd_ump_midi1 {
	struct snd_ump_midi1_msg_note note;
	struct snd_ump_midi1_msg_paf paf;
	struct snd_ump_midi1_msg_cc cc;
	struct snd_ump_midi1_msg_program pg;
	struct snd_ump_midi1_msg_caf caf;
	struct snd_ump_midi1_msg_pitchbend pb;
	struct snd_ump_system_msg system;
	__le32 raw;
};

/* MIDI 2.0 Note Off / Note On */
struct snd_ump_midi2_msg_note {
	__u8 attribute_type;
	__u8 note;
	__u8 status_channel;
	__u8 type_group;
	__le16 attribute_data;
	__le16 velocity;
} __packed;

/* MIDI 2.0 Poly Pressure */
struct snd_ump_midi2_msg_paf {
	__u8 reserved;
	__u8 note;
	__u8 status_channel;
	__u8 type_group;
	__u32 data;
} __packed;

/* MIDI 2.0 Per-Note Controller */
struct snd_ump_midi2_msg_pernote_cc {
	__u8 index;
	__u8 note;
	__u8 status_channel;
	__u8 type_group;
	__u32 data;
} __packed;

/* MIDI 2.0 Per-Note Management */
struct snd_ump_midi2_msg_pernote_mgmt {
	__u8 flags;
	__u8 note;
	__u8 status_channel;
	__u8 type_group;
	__u32 reserved;
} __packed;

/* MIDI 2.0 Control Change */
struct snd_ump_midi2_msg_cc {
	__u8 reserved;
	__u8 index;
	__u8 status_channel;
	__u8 type_group;
	__u32 data;
} __packed;

/* MIDI 2.0 RPN / NRPN */
struct snd_ump_midi2_msg_rpn {
	__u8 index;
	__u8 bank;
	__u8 status_channel;
	__u8 type_group;
	__u32 data;
} __packed;

/* MIDI 2.0 Program Change */
struct snd_ump_midi2_msg_program {
	__u8 flags;
	__u8 reserved;
	__u8 status_channel;
	__u8 type_group;
	__u8 bank_lsb;
	__u8 bank_msb;
	__u8 reserved2;
	__u8 program;
} __packed;

/* MIDI 2.0 Channel Pressure */
struct snd_ump_midi2_msg_caf {
	__u16 reserved;
	__u8 status_channel;
	__u8 type_group;
	__u32 data;
} __packed;

/* MIDI 2.0 Pitch Bend */
struct snd_ump_midi2_msg_pitchbend {
	__u16 reserved;
	__u8 status_channel;
	__u8 type_group;
	__u32 data;
} __packed;

/* MIDI 2.0 Per-Note Pitch Bend */
struct snd_ump_midi2_msg_pernote_pitchbend {
	__u8 reserved;
	__u8 note;
	__u8 status_channel;
	__u8 type_group;
	__u32 data;
} __packed;

/* MIDI 2.0 UMP CMV */
union snd_ump_midi2 {
	struct snd_ump_midi2_msg_note note;
	struct snd_ump_midi2_msg_paf paf;
	struct snd_ump_midi2_msg_pernote_cc pernote_cc;
	struct snd_ump_midi2_msg_pernote_mgmt pernote_mgmt;
	struct snd_ump_midi2_msg_cc cc;
	struct snd_ump_midi2_msg_rpn rpn;
	struct snd_ump_midi2_msg_program pg;
	struct snd_ump_midi2_msg_caf caf;
	struct snd_ump_midi2_msg_pitchbend pb;
	struct snd_ump_midi2_msg_pernote_pitchbend pernote_pb;
	__le32 raw[2];
};

int snd_seq_deliver_from_ump(struct snd_seq_client *dest,
			     struct snd_seq_client_port *dest_port,
			     struct snd_seq_event *event,
			     int atomic, int hop);
int snd_seq_deliver_to_ump(struct snd_seq_client *dest,
			   struct snd_seq_client_port *dest_port,
			   struct snd_seq_event *event,
			   int atomic, int hop);

#endif /* __SEQ_UMP_CONVERT_H */
