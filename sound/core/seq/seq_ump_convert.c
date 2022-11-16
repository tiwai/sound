// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * ALSA sequencer event conversion between UMP and legacy clients
 */

#include <linux/init.h>
#include <linux/errno.h>
#include <linux/string.h>
#include <sound/core.h>
#include <sound/ump.h>
#include "seq_ump_convert.h"

/* number of 32bit words for each UMP message type */
static unsigned char ump_packet_words[0x10] = {
	1, 1, 1, 2, 2, 4, 1, 1, 2, 2, 2, 3, 3, 4, 4, 4
};

/*
 * Upgrade / downgrade value bits (also do endian conversion)
 */
static u8 downscale_32_to_7bit(unsigned int src)
{
	return le32_to_cpu(src >> 25);
}

static u16 downscale_32_to_14bit(__le32 src)
{
	return le32_to_cpu(src) >> 18;
}

static u8 downscale_16_to_7bit(__le16 src)
{
	return le16_to_cpu(src) >> 9;
}

static __le16 upscale_7_to_16bit(u8 src)
{
	u16 val, repeat;

	val = (u16)src << 9;
	if (src <= 0x40)
		return cpu_to_le16(val);
	repeat = src & 0x3f;
	return cpu_to_le16(val | (repeat << 3) | (repeat >> 3));
}

static __le32 upscale_7_to_32bit(u8 src)
{
	u32 val, repeat;

	val = src << 25;
	if (src <= 0x40)
		return cpu_to_le32(val);
	repeat = src & 0x3f;
	return cpu_to_le32(val | (repeat << 19) | (repeat << 13) |
			   (repeat << 7) | (repeat << 1) | (repeat >> 5));
}

static __le32 upscale_14_to_32bit(u16 src)
{
	u32 val, repeat;

	val = src << 18;
	if (src <= 0x2000)
		return cpu_to_le32(val);
	repeat = src & 0x1fff;
	return cpu_to_le32(val | (repeat << 5) | (repeat >> 8));
}

#define get_ump_channel(v)	ump_message_channel(le32_to_cpu(v))
#define get_ump_type(v)		ump_message_type(le32_to_cpu(v))
#define get_ump_status(v)	ump_message_status_code(le32_to_cpu(v))
#define get_ump_group(v)	ump_message_group(le32_to_cpu(v))
#define make_ump(type, group) cpu_to_le32(ump_compose(type, group, 0))

/*
 * UMP -> MIDI1 sequencer event
 */

/* MIDI 1.0 CVM */

/* encode note event */
static void ump_midi1_to_note_ev(const union snd_ump_midi1 *val,
				 struct snd_seq_event *ev)
{
	ev->data.note.channel = get_ump_channel(val->raw);
	ev->data.note.note = val->note.note;
	ev->data.note.velocity = val->note.velocity;
}

/* encode one parameter controls */
static void ump_midi1_to_ctrl_ev(const union snd_ump_midi1 *val,
				 struct snd_seq_event *ev)
{
	ev->data.control.channel = get_ump_channel(val->raw);
	ev->data.control.value = val->caf.data;
}

/* encode pitch wheel change */
static void ump_midi1_to_pitchbend_ev(const union snd_ump_midi1 *val,
				      struct snd_seq_event *ev)
{
	ev->data.control.channel = get_ump_channel(val->raw);
	ev->data.control.value = (val->pb.data_msb << 7) | val->pb.data_lsb;
	ev->data.control.value -= 8192;
}

/* encode midi control change */
static void ump_midi1_to_cc_ev(const union snd_ump_midi1 *val,
			       struct snd_seq_event *ev)
{
	ev->data.control.channel = get_ump_channel(val->raw);
	ev->data.control.param = val->cc.index;
	ev->data.control.value = val->cc.data;
}

/* Encoding MIDI 1.0 UMP packet */
struct seq_ump_midi1_to_ev {
	int seq_type;
	void (*encode)(const union snd_ump_midi1 *val,
		       struct snd_seq_event *ev);
};

/* Encoders for MIDI1 status 0x80-0xe0 */
static struct seq_ump_midi1_to_ev midi1_msg_encoders[] = {
	{SNDRV_SEQ_EVENT_NOTEOFF,	ump_midi1_to_note_ev},	/* 0x80 */
	{SNDRV_SEQ_EVENT_NOTEON,	ump_midi1_to_note_ev},	/* 0x90 */
	{SNDRV_SEQ_EVENT_KEYPRESS,	ump_midi1_to_note_ev},	/* 0xa0 */
	{SNDRV_SEQ_EVENT_CONTROLLER,	ump_midi1_to_cc_ev},	/* 0xb0 */
	{SNDRV_SEQ_EVENT_PGMCHANGE,	ump_midi1_to_ctrl_ev},	/* 0xc0 */
	{SNDRV_SEQ_EVENT_CHANPRESS,	ump_midi1_to_ctrl_ev},	/* 0xd0 */
	{SNDRV_SEQ_EVENT_PITCHBEND,	ump_midi1_to_pitchbend_ev}, /* 0xe0 */
};

static int cvt_ump_midi1_to_event(const union snd_ump_midi1 *val,
				  struct snd_seq_event *ev)
{
	unsigned char status = get_ump_status(val->raw);

	if (status < 0x80 || status > 0xe0)
		return 0; /* invalid - skip */
	status = (status >> 4) - 8;
	ev->type = midi1_msg_encoders[status].seq_type;
	ev->source.port = get_ump_group(val->raw);
	ev->flags = SNDRV_SEQ_EVENT_LENGTH_FIXED;
	midi1_msg_encoders[status].encode(val, ev);
	return 1;
}

/* MIDI System message */

/* encode one parameter value*/
static void ump_system_to_one_param_ev(const union snd_ump_midi1 *val,
				       struct snd_seq_event *ev)
{
	ev->data.control.value = val->system.parm1;
}

/* encode song position */
static void ump_system_to_songpos_ev(const union snd_ump_midi1 *val,
				     struct snd_seq_event *ev)
{
	ev->data.control.value = (val->system.parm1 << 7) | val->system.parm2;
}

/* Encoders for 0xf0 - 0xff */
static struct seq_ump_midi1_to_ev system_msg_encoders[] = {
	{SNDRV_SEQ_EVENT_NONE,		NULL},	 /* 0xf0 */
	{SNDRV_SEQ_EVENT_QFRAME,	ump_system_to_one_param_ev}, /* 0xf1 */
	{SNDRV_SEQ_EVENT_SONGPOS,	ump_system_to_songpos_ev}, /* 0xf2 */
	{SNDRV_SEQ_EVENT_SONGSEL,	ump_system_to_one_param_ev}, /* 0xf3 */
	{SNDRV_SEQ_EVENT_NONE,		NULL}, /* 0xf4 */
	{SNDRV_SEQ_EVENT_NONE,		NULL}, /* 0xf5 */
	{SNDRV_SEQ_EVENT_TUNE_REQUEST,	NULL}, /* 0xf6 */
	{SNDRV_SEQ_EVENT_NONE,		NULL}, /* 0xf7 */
	{SNDRV_SEQ_EVENT_CLOCK,		NULL}, /* 0xf8 */
	{SNDRV_SEQ_EVENT_NONE,		NULL}, /* 0xf9 */
	{SNDRV_SEQ_EVENT_START,		NULL}, /* 0xfa */
	{SNDRV_SEQ_EVENT_CONTINUE,	NULL}, /* 0xfb */
	{SNDRV_SEQ_EVENT_STOP,		NULL}, /* 0xfc */
	{SNDRV_SEQ_EVENT_NONE,		NULL}, /* 0xfd */
	{SNDRV_SEQ_EVENT_SENSING,	NULL}, /* 0xfe */
	{SNDRV_SEQ_EVENT_RESET,		NULL}, /* 0xff */
};

static int cvt_ump_system_to_event(const union snd_ump_midi1 *val,
				   struct snd_seq_event *ev)
{
	unsigned char status = get_ump_status(val->raw);

	if (status < UMP_SYSTEM_STATUS_MASK)
		return 0; /* invalid - skip */
	status &= ~UMP_SYSTEM_STATUS_MASK;
	ev->type = system_msg_encoders[status].seq_type;
	ev->source.port = get_ump_group(val->raw);
	ev->flags = SNDRV_SEQ_EVENT_LENGTH_FIXED;
	if (ev->type == SNDRV_SEQ_EVENT_NONE)
		return 0;
	if (system_msg_encoders[status].encode)
		system_msg_encoders[status].encode(val, ev);
	return 1;
}

/* MIDI 2.0 CVM */

/* encode note event */
static int ump_midi2_to_note_ev(const union snd_ump_midi2 *val,
				struct snd_seq_event *ev)
{
	ev->data.note.channel = get_ump_channel(*val->raw);
	ev->data.note.note = val->note.note;
	ev->data.note.velocity = downscale_16_to_7bit(val->note.velocity);
	/* correct note-on velocity 0 to 1;
	 * it's no longer equivalent as not-off for MIDI 2.0
	 */
	if (ev->type == SNDRV_SEQ_EVENT_NOTEON &&
	    !ev->data.note.velocity)
		ev->data.note.velocity = 1;
	return 1;
}

/* encode pitch wheel change */
static int ump_midi2_to_pitchbend_ev(const union snd_ump_midi2 *val,
				     struct snd_seq_event *ev)
{
	ev->data.control.channel = get_ump_channel(*val->raw);
	ev->data.control.value = downscale_32_to_14bit(val->pb.data);
	ev->data.control.value -= 8192;
	return 1;
}

/* encode midi control change */
static int ump_midi2_to_cc_ev(const union snd_ump_midi2 *val,
			      struct snd_seq_event *ev)
{
	ev->data.control.channel = get_ump_channel(*val->raw);
	ev->data.control.param = val->cc.index;
	ev->data.control.value = downscale_32_to_7bit(val->cc.data);
	return 1;
}

/* encode midi program change */
static int ump_midi2_to_pgm_ev(const union snd_ump_midi2 *val,
			       struct snd_seq_event *ev)
{
	int size = 1;

	ev->data.control.channel = get_ump_channel(*val->raw);
	if (val->pg.flags & UMP_PROGRAM_CHANGE_BANK_VALID) {
		ev->type = SNDRV_SEQ_EVENT_CONTROL14;
		ev->data.control.param = UMP_CC_BANK_SELECT;
		ev->data.control.value = (val->pg.bank_msb << 7) | val->pg.bank_lsb;
		ev[1] = ev[0];
		ev++;
		ev->type = SNDRV_SEQ_EVENT_PGMCHANGE;
		size = 2;
	}
	ev->data.control.value = val->pg.program;
	return size;
}

/* encode one parameter controls */
static int ump_midi2_to_ctrl_ev(const union snd_ump_midi2 *val,
				struct snd_seq_event *ev)
{
	ev->data.control.channel = get_ump_channel(*val->raw);
	ev->data.control.value = downscale_32_to_7bit(val->caf.data);
	return 1;
}

/* encode RPN/NRPN */
static int ump_midi2_to_rpn_ev(const union snd_ump_midi2 *val,
			       struct snd_seq_event *ev)
{
	ev->data.control.channel = get_ump_channel(*val->raw);
	ev->data.control.param = (val->rpn.bank << 7) | val->rpn.index;
	ev->data.control.value = downscale_32_to_14bit(val->rpn.data);
	return 1;
}

/* Encoding MIDI 2.0 UMP Packet */
struct seq_ump_midi2_to_ev {
	int seq_type;
	int (*encode)(const union snd_ump_midi2 *val,
		      struct snd_seq_event *ev);
};

/* Encoders for MIDI2 status 0x00-0xf0 */
static struct seq_ump_midi2_to_ev midi2_msg_encoders[] = {
	{SNDRV_SEQ_EVENT_NONE,		NULL},			/* 0x00 */
	{SNDRV_SEQ_EVENT_NONE,		NULL},			/* 0x10 */
	{SNDRV_SEQ_EVENT_REGPARAM,	ump_midi2_to_rpn_ev},	/* 0x20 */
	{SNDRV_SEQ_EVENT_NONREGPARAM,	ump_midi2_to_rpn_ev},	/* 0x30 */
	{SNDRV_SEQ_EVENT_NONE,		NULL},			/* 0x40 */
	{SNDRV_SEQ_EVENT_NONE,		NULL},			/* 0x50 */
	{SNDRV_SEQ_EVENT_NONE,		NULL},			/* 0x60 */
	{SNDRV_SEQ_EVENT_NONE,		NULL},			/* 0x70 */
	{SNDRV_SEQ_EVENT_NOTEOFF,	ump_midi2_to_note_ev},	/* 0x80 */
	{SNDRV_SEQ_EVENT_NOTEON,	ump_midi2_to_note_ev},	/* 0x90 */
	{SNDRV_SEQ_EVENT_KEYPRESS,	ump_midi2_to_note_ev},	/* 0xa0 */
	{SNDRV_SEQ_EVENT_CONTROLLER,	ump_midi2_to_cc_ev},	/* 0xb0 */
	{SNDRV_SEQ_EVENT_PGMCHANGE,	ump_midi2_to_pgm_ev},	/* 0xc0 */
	{SNDRV_SEQ_EVENT_CHANPRESS,	ump_midi2_to_ctrl_ev},	/* 0xd0 */
	{SNDRV_SEQ_EVENT_PITCHBEND,	ump_midi2_to_pitchbend_ev}, /* 0xe0 */
	{SNDRV_SEQ_EVENT_NONE,		NULL},			/* 0xf0 */
};

static int cvt_ump_midi2_to_event(const union snd_ump_midi2 *val,
				  struct snd_seq_event *ev)
{
	unsigned char status = get_ump_status(*val->raw);

	status >>= 4;
	ev->type = midi2_msg_encoders[status].seq_type;
	if (ev->type == SNDRV_SEQ_EVENT_NONE)
		return 0; /* skip */
	ev->flags = SNDRV_SEQ_EVENT_LENGTH_FIXED;
	ev->source.port = get_ump_group(*val->raw);
	return midi2_msg_encoders[status].encode(val, ev);
}

/* parse and compose for a sysex var-length event */
static int cvt_ump_sysex7_to_event(const __le32 *data, unsigned char *buf,
				   struct snd_seq_event *ev)
{
	u32 val = le32_to_cpu(*data);
	unsigned char status;
	unsigned char bytes;
	int size = 0;

	status = ump_sysex_message_status(val);
	bytes = ump_sysex_message_length(val);
	if (bytes > 6)
		return 0; // skip

	if (status == UMP_SYSEX_STATUS_SINGLE ||
	    status == UMP_SYSEX_STATUS_START) {
		buf[0] = UMP_MSG_MIDI1_SYSEX_START;
		size = 1;
	}

	if (bytes > 0)
		buf[size++] = (val >> 8) & 0x7f;
	if (bytes > 1)
		buf[size++] = val & 0x7f;
	val = le32_to_cpu(data[1]);
	if (bytes > 2)
		buf[size++] = (val >> 24) & 0x7f;
	if (bytes > 3)
		buf[size++] = (val >> 16) & 0x7f;
	if (bytes > 4)
		buf[size++] = (val >> 8) & 0x7f;
	if (bytes > 5)
		buf[size++] = val & 0x7f;

	if (status == UMP_SYSEX_STATUS_SINGLE ||
	    status == UMP_SYSEX_STATUS_END)
		buf[size++] = UMP_MSG_MIDI1_SYSEX_END;

	ev->type = SNDRV_SEQ_EVENT_SYSEX;
	ev->flags = SNDRV_SEQ_EVENT_LENGTH_VARIABLE;
	ev->data.ext.len = size;
	ev->data.ext.ptr = buf;
	return 1;
}

/* convert UMP packet from MIDI 1.0 to MIDI 2.0 and deliver it */
static int cvt_ump_midi1_to_midi2(struct snd_seq_client *dest,
				  struct snd_seq_client_port *dest_port,
				  struct snd_seq_event *event,
				  int atomic, int hop)
{
	struct snd_seq_event ev_cvt;
	union snd_ump_midi1 midi1;
	union snd_ump_midi2 *midi2 = (union snd_ump_midi2 *)ev_cvt.data.ump.d;

	ev_cvt = *event;
	memset(&ev_cvt.data, 0, sizeof(ev_cvt.data));
	midi1.raw = event->data.ump.d[0];
	midi2->note.type_group = (UMP_MSG_TYPE_MIDI2 << 4) |
		(midi1.note.type_group & 0x0f);
	midi2->note.status_channel = midi1.note.status_channel;
	switch (midi1.note.status_channel & 0xf0) {
	case UMP_MSG_STATUS_NOTE_ON:
	case UMP_MSG_STATUS_NOTE_OFF:
		midi2->note.note = midi1.note.note;
		midi2->note.velocity = upscale_7_to_16bit(midi1.note.velocity);
		break;
	case UMP_MSG_STATUS_POLY_PRESSURE:
		midi2->paf.note = midi1.note.note;
		midi2->paf.data = upscale_7_to_32bit(midi1.note.velocity);
		break;
	case UMP_MSG_STATUS_CC:
		midi2->cc.index = midi1.cc.index;
		midi2->cc.data = upscale_7_to_32bit(midi1.cc.data);
		break;
	case UMP_MSG_STATUS_PROGRAM:
		midi2->pg.program = midi1.caf.data;
		break;
	case UMP_MSG_STATUS_CHANNEL_PRESSURE:
		midi2->caf.data = upscale_7_to_32bit(midi1.caf.data);
		break;
	case UMP_MSG_STATUS_PITCH_BEND:
		midi2->pb.data = upscale_14_to_32bit((midi1.pb.data_msb << 7) |
						     midi1.pb.data_lsb);
		break;
	default:
		return 0;
	}

	return __snd_seq_deliver_single_event(dest, dest_port,
					      &ev_cvt, atomic, hop);
}

/* convert UMP packet from MIDI 2.0 to MIDI 1.0 and deliver it */
static int cvt_ump_midi2_to_midi1(struct snd_seq_client *dest,
				  struct snd_seq_client_port *dest_port,
				  struct snd_seq_event *event,
				  int atomic, int hop)
{
	struct snd_seq_event ev_cvt;
	union snd_ump_midi1 midi1;
	union snd_ump_midi2 *midi2 = (union snd_ump_midi2 *)event->data.ump.d;
	u16 v;

	midi1.raw = 0;
	midi1.note.type_group = (UMP_MSG_TYPE_MIDI1 << 4) |
		(midi2->note.type_group & 0x0f);
	midi1.note.status_channel = midi2->note.status_channel;
	switch (midi1.note.status_channel & 0xf0) {
	case UMP_MSG_STATUS_NOTE_ON:
	case UMP_MSG_STATUS_NOTE_OFF:
		midi1.note.note = midi2->note.note;
		midi1.note.velocity = downscale_16_to_7bit(midi2->note.velocity);
		break;
	case UMP_MSG_STATUS_POLY_PRESSURE:
		midi1.note.note = midi2->paf.note;
		midi1.note.velocity = downscale_32_to_7bit(midi2->paf.data);
		break;
	case UMP_MSG_STATUS_CC:
		midi1.cc.index = midi2->cc.index;
		midi1.cc.data = downscale_32_to_7bit(midi1.cc.data);
		break;
	case UMP_MSG_STATUS_PROGRAM:
		midi1.caf.data = midi2->pg.program;
		break;
	case UMP_MSG_STATUS_CHANNEL_PRESSURE:
		midi1.caf.data = downscale_32_to_7bit(midi2->caf.data);
		break;
	case UMP_MSG_STATUS_PITCH_BEND:
		v = downscale_32_to_14bit(midi2->pb.data);
		midi1.pb.data_msb = v >> 7;
		midi1.pb.data_lsb = v & 0x7f;
		break;
	default:
		return 0;
	}

	ev_cvt = *event;
	memset(&ev_cvt.data, 0, sizeof(ev_cvt.data));
	ev_cvt.data.ump.d[0] = midi1.raw;

	return __snd_seq_deliver_single_event(dest, dest_port,
					      &ev_cvt, atomic, hop);
}

/* convert UMP to a legacy ALSA seq event and deliver it */
static int cvt_ump_to_any(struct snd_seq_client *dest,
			  struct snd_seq_client_port *dest_port,
			  struct snd_seq_event *event,
			  const __le32 *data,
			  unsigned char type,
			  int atomic, int hop)
{
	struct snd_seq_event ev_cvt[2]; /* up to two events */
	/* use the second event as a temp buffer for saving stack usage */
	unsigned char *sysex_buf = (unsigned char *)(ev_cvt + 1);
	int i, len, err;

	ev_cvt[0] = ev_cvt[1] = *event;
	switch (type) {
	case UMP_MSG_TYPE_SYSTEM:
		len = cvt_ump_system_to_event((union snd_ump_midi1 *)data,
					      ev_cvt);
		break;
	case UMP_MSG_TYPE_MIDI1:
		len = cvt_ump_midi1_to_event((union snd_ump_midi1 *)data,
					     ev_cvt);
		break;
	case UMP_MSG_TYPE_MIDI2:
		len = cvt_ump_midi2_to_event((union snd_ump_midi2 *)data,
					     ev_cvt);
		break;
	case UMP_MSG_TYPE_SYSEX7:
		len = cvt_ump_sysex7_to_event(data, sysex_buf, ev_cvt);
		break;
	default:
		return 0;
	}

	for (i = 0; i < len; i++) {
		err = __snd_seq_deliver_single_event(dest, dest_port,
						     &ev_cvt[i], atomic, hop);
		if (err < 0)
			return err;
	}

	return 0;
}

/* Convert from UMP packet and deliver */
int snd_seq_deliver_from_ump(struct snd_seq_client *dest,
			     struct snd_seq_client_port *dest_port,
			     struct snd_seq_event *event,
			     int atomic, int hop)
{
	unsigned char type;
	const __le32 *data;
	__le32 ext_data[4];
	int err;

	if (snd_seq_ev_is_variable(event)) {
		if ((event->data.ext.len & ~SNDRV_SEQ_EXT_MASK) % 8)
			return 0; // invalid - skip
		if (snd_seq_client_is_ump(dest)) /* shortcut: copy as-is */
			return __snd_seq_deliver_single_event(dest, dest_port,
							      event, atomic, hop);
		err = snd_seq_expand_var_event(event, sizeof(ext_data),
					       (char *)ext_data, true, 0);
		if (err <= 0)
			return 0;
		data = ext_data;
		type = get_ump_type(*data);
	} else {
		data = event->data.ump.d;
		type = get_ump_type(*data);
		if (ump_packet_words[type] > 3)
			return 0;
	}

	if (snd_seq_client_is_ump(dest)) {
		if (snd_seq_client_is_midi2(dest) && type == UMP_MSG_TYPE_MIDI1)
			return cvt_ump_midi1_to_midi2(dest, dest_port,
						      event, atomic, hop);
		else if (!snd_seq_client_is_midi2(dest) && type == UMP_MSG_TYPE_MIDI2)
			return cvt_ump_midi2_to_midi1(dest, dest_port,
						      event, atomic, hop);
		/* copy as-is */
		return __snd_seq_deliver_single_event(dest, dest_port,
						      event, atomic, hop);
	}

	return cvt_ump_to_any(dest, dest_port, event, data, type, atomic, hop);
}

/*
 * MIDI1 sequencer event -> UMP conversion
 */

/* Conversion to UMP MIDI 1.0 */

/* convert note on/off event to MIDI 1.0 UMP */
static int note_ev_to_ump_midi1(const struct snd_seq_event *event,
				union snd_ump_midi1 *data,
				unsigned char status)
{
	data->note.status_channel = status | event->data.note.channel;
	data->note.velocity = event->data.note.velocity;
	data->note.note = event->data.note.note;
	return 1;
}

/* convert CC event to MIDI 1.0 UMP */
static int cc_ev_to_ump_midi1(const struct snd_seq_event *event,
			      union snd_ump_midi1 *data,
			      unsigned char status)
{
	data->cc.status_channel = status | event->data.control.channel;
	data->cc.index = event->data.control.param;
	data->cc.data = event->data.control.value;
	return 1;
}

/* convert one-parameter control event to MIDI 1.0 UMP */
static int ctrl_ev_to_ump_midi1(const struct snd_seq_event *event,
				union snd_ump_midi1 *data, unsigned char status)
{
	data->caf.status_channel = status | event->data.control.channel;
	data->caf.data = event->data.control.value;
	return 1;
}

/* convert pitchbend event to MIDI 1.0 UMP */
static int pitchbend_ev_to_ump_midi1(const struct snd_seq_event *event,
				     union snd_ump_midi1 *data,
				     unsigned char status)
{
	u16 val = event->data.control.value + 8192;

	data->pb.status_channel = status | event->data.control.channel;
	data->pb.data_msb = (val >> 7) & 0x7f;
	data->pb.data_lsb = val & 0x7f;
	return 1;
}

/* convert 14bit control event to MIDI 1.0 UMP; split to two events */
static int ctrl14_ev_to_ump_midi1(const struct snd_seq_event *event,
				  union snd_ump_midi1 *data,
				  unsigned char status)
{
	data->cc.status_channel = UMP_MSG_STATUS_CC | event->data.control.channel;
	if (event->data.control.param < 0x20) {
		data->cc.index = event->data.control.param;
		data->cc.data = (event->data.control.value >> 7) & 0x7f;
		data[1] = data[0];
		data[1].cc.index = event->data.control.param | 0x20;
		data[1].cc.data = event->data.control.value & 0x7f;
		return 2;
	}

	data->cc.index = event->data.control.param;
	data->cc.data = event->data.control.value & 0x7f;
	return 1;
}

/* convert RPN/NRPN event to MIDI 1.0 UMP; split to four events */
static int rpn_ev_to_ump_midi1(const struct snd_seq_event *event,
			       union snd_ump_midi1 *data,
			       unsigned char status)
{
	bool is_rpn = (status == UMP_MSG_STATUS_RPN);

	data->cc.status_channel = UMP_MSG_STATUS_CC | event->data.control.channel;
	data[1] = data[2] = data[3] = data[0];

	data[0].cc.index = is_rpn ? UMP_CC_RPN_MSB : UMP_CC_NRPN_MSB;
	data[0].cc.data = (event->data.control.param >> 7) & 0x7f;
	data[1].cc.index = is_rpn ? UMP_CC_RPN_LSB : UMP_CC_NRPN_LSB;
	data[1].cc.data = event->data.control.param & 0x7f;
	data[2].cc.index = UMP_CC_DATA;
	data[2].cc.data = (event->data.control.value >> 7) & 0x7f;
	data[3].cc.index = UMP_CC_DATA_LSB;
	data[3].cc.data = event->data.control.value & 0x7f;
	return 4;
}

/* Conversion to UMP MIDI 2.0 */

/* convert note on/off event to MIDI 2.0 UMP */
static int note_ev_to_ump_midi2(const struct snd_seq_event *event,
				union snd_ump_midi2 *data,
				unsigned char status)
{
	data->note.status_channel = status | event->data.note.channel;
	data->note.note = event->data.note.note;
	data->note.velocity = upscale_7_to_16bit(event->data.note.velocity);
	return 1;
}

/* convert PAF event to MIDI 2.0 UMP */
static int paf_ev_to_ump_midi2(const struct snd_seq_event *event,
			       union snd_ump_midi2 *data,
			       unsigned char status)
{
	data->paf.status_channel = status | event->data.note.channel;
	data->paf.note = event->data.note.note;
	data->paf.data = upscale_7_to_32bit(event->data.note.velocity);
	return 1;
}

/* convert CC event to MIDI 2.0 UMP */
static int cc_ev_to_ump_midi2(const struct snd_seq_event *event,
			      union snd_ump_midi2 *data,
			      unsigned char status)
{
	data->cc.status_channel = status | event->data.control.channel;
	data->cc.index = event->data.control.param;
	data->cc.data = upscale_7_to_32bit(event->data.control.value);
	return 1;
}

/* convert one-parameter control event to MIDI 2.0 UMP */
static int ctrl_ev_to_ump_midi2(const struct snd_seq_event *event,
				union snd_ump_midi2 *data, unsigned char status)
{
	data->caf.status_channel = status | event->data.control.channel;
	data->caf.data = upscale_7_to_32bit(event->data.control.value);
	return 1;
}

/* convert program change event to MIDI 2.0 UMP */
static int pgm_ev_to_ump_midi2(const struct snd_seq_event *event,
			       union snd_ump_midi2 *data, unsigned char status)
{
	data->pg.status_channel = status | event->data.control.channel;
	data->pg.program = event->data.control.value;
	return 1;
}

/* convert pitchbend event to MIDI 2.0 UMP */
static int pitchbend_ev_to_ump_midi2(const struct snd_seq_event *event,
				     union snd_ump_midi2 *data,
				     unsigned char status)
{
	u16 val = event->data.control.value + 8192;

	data->pb.status_channel = status | event->data.control.channel;
	data->pb.data = upscale_14_to_32bit(val);
	return 1;
}

/* convert 14bit control event to MIDI 2.0 UMP; split to two events */
static int ctrl14_ev_to_ump_midi2(const struct snd_seq_event *event,
				  union snd_ump_midi2 *data,
				  unsigned char status)
{
	data->cc.status_channel = UMP_MSG_STATUS_CC | event->data.control.channel;
	if (event->data.control.param < 0x20) {
		data->cc.index = event->data.control.param;
		data->cc.data = upscale_7_to_32bit((event->data.control.value >> 7) & 0x7f);
		data[1] = data[0];
		data[1].cc.index = event->data.control.param | 0x20;
		data[1].cc.data = upscale_7_to_32bit(event->data.control.value & 0x7f);
		return 2;
	}

	data->cc.index = event->data.control.param;
	data->cc.data = upscale_7_to_32bit(event->data.control.value & 0x7f);
	return 1;
}

/* convert RPN/NRPN event to MIDI 2.0 UMP */
static int rpn_ev_to_ump_midi2(const struct snd_seq_event *event,
			       union snd_ump_midi2 *data,
			       unsigned char status)
{
	data->rpn.status_channel = status | event->data.control.channel;
	data->rpn.bank = (event->data.control.param >> 7) & 0x7f;
	data->rpn.index = event->data.control.param & 0x7f;
	data->rpn.data = upscale_14_to_32bit(event->data.control.value);
	return 1;
}

struct seq_ev_to_ump {
	int seq_type;
	unsigned char status;
	int (*midi1_encode)(const struct snd_seq_event *event,
			    union snd_ump_midi1 *data,
			    unsigned char status);
	int (*midi2_encode)(const struct snd_seq_event *event,
			    union snd_ump_midi2 *data,
			    unsigned char status);
};

static const struct seq_ev_to_ump seq_ev_ump_encoders[] = {
	{ SNDRV_SEQ_EVENT_NOTEON, UMP_MSG_STATUS_NOTE_ON,
	  note_ev_to_ump_midi1, note_ev_to_ump_midi2 },
	{ SNDRV_SEQ_EVENT_NOTEOFF, UMP_MSG_STATUS_NOTE_OFF,
	  note_ev_to_ump_midi1, note_ev_to_ump_midi2 },
	{ SNDRV_SEQ_EVENT_KEYPRESS, UMP_MSG_STATUS_POLY_PRESSURE,
	  note_ev_to_ump_midi1, paf_ev_to_ump_midi2 },
	{ SNDRV_SEQ_EVENT_CONTROLLER, UMP_MSG_STATUS_CC,
	  cc_ev_to_ump_midi1, cc_ev_to_ump_midi2 },
	{ SNDRV_SEQ_EVENT_PGMCHANGE, UMP_MSG_STATUS_PROGRAM,
	  ctrl_ev_to_ump_midi1, pgm_ev_to_ump_midi2 },
	{ SNDRV_SEQ_EVENT_CHANPRESS, UMP_MSG_STATUS_CHANNEL_PRESSURE,
	  ctrl_ev_to_ump_midi1, ctrl_ev_to_ump_midi2 },
	{ SNDRV_SEQ_EVENT_PITCHBEND, UMP_MSG_STATUS_PITCH_BEND,
	  pitchbend_ev_to_ump_midi1, pitchbend_ev_to_ump_midi2 },
	{ SNDRV_SEQ_EVENT_CONTROL14, 0,
	  ctrl14_ev_to_ump_midi1, ctrl14_ev_to_ump_midi2 },
	{ SNDRV_SEQ_EVENT_NONREGPARAM, UMP_MSG_STATUS_NRPN,
	  rpn_ev_to_ump_midi1, rpn_ev_to_ump_midi2 },
	{ SNDRV_SEQ_EVENT_REGPARAM, UMP_MSG_STATUS_RPN,
	  rpn_ev_to_ump_midi1, rpn_ev_to_ump_midi2 },
};

static const struct seq_ev_to_ump *find_ump_encoder(int type)
{
	int i;

	for (i = 0; i < ARRAY_SIZE(seq_ev_ump_encoders); i++)
		if (seq_ev_ump_encoders[i].seq_type == type)
			return &seq_ev_ump_encoders[i];

	return NULL;
}

/* Convert ALSA seq event to UMP MIDI 1.0 and deliver it */
static int cvt_to_ump_midi1(struct snd_seq_client *dest,
			    struct snd_seq_client_port *dest_port,
			    struct snd_seq_event *event,
			    int atomic, int hop)
{
	const struct seq_ev_to_ump *encoder;
	struct snd_seq_event ev_cvt;
	union snd_ump_midi1 data[4];
	int i, n, err;

	encoder = find_ump_encoder(event->type);
	if (!encoder)
		return __snd_seq_deliver_single_event(dest, dest_port,
						      event, atomic, hop);

	data->raw = make_ump(UMP_MSG_TYPE_MIDI1, event->source.port);
	n = encoder->midi1_encode(event, data, encoder->status);
	if (!n)
		return 0;

	ev_cvt = *event;
	ev_cvt.type = SNDRV_SEQ_EVENT_UMP;
	ev_cvt.flags &= ~SNDRV_SEQ_EVENT_LENGTH_MASK;
	memset(&ev_cvt.data, 0, sizeof(ev_cvt.data));
	for (i = 0; i < n; i++) {
		ev_cvt.data.ump.d[0] = data[i].raw;
		err = __snd_seq_deliver_single_event(dest, dest_port,
						     &ev_cvt, atomic, hop);
		if (err < 0)
			return err;
	}

	return 0;
}

/* Convert ALSA seq event to UMP MIDI 2.0 and deliver it */
static int cvt_to_ump_midi2(struct snd_seq_client *dest,
			    struct snd_seq_client_port *dest_port,
			    struct snd_seq_event *event,
			    int atomic, int hop)
{
	const struct seq_ev_to_ump *encoder;
	struct snd_seq_event ev_cvt;
	union snd_ump_midi2 data[2];
	int i, n, err;

	encoder = find_ump_encoder(event->type);
	if (!encoder)
		return __snd_seq_deliver_single_event(dest, dest_port,
						      event, atomic, hop);

	data->raw[0] = make_ump(UMP_MSG_TYPE_MIDI2, event->source.port);
	data->raw[1] = 0;
	n = encoder->midi2_encode(event, data, encoder->status);
	if (!n)
		return 0;

	ev_cvt = *event;
	ev_cvt.type = SNDRV_SEQ_EVENT_UMP;
	ev_cvt.flags &= ~SNDRV_SEQ_EVENT_LENGTH_MASK;
	memset(&ev_cvt.data, 0, sizeof(ev_cvt.data));
	for (i = 0; i < n; i++) {
		ev_cvt.data.raw32.d[0] = data[i].raw[0];
		ev_cvt.data.raw32.d[1] = data[i].raw[1];
		err = __snd_seq_deliver_single_event(dest, dest_port,
						     &ev_cvt, atomic, hop);
		if (err < 0)
			return err;
	}

	return 0;
}

/* Fill up a sysex7 UMP from the byte stream */
static void fill_sysex7_ump(__le32 *data, u8 group, u8 status, u8 *buf, int len)
{
	u32 val[2];

	memset(val, 0, sizeof(val));
	memcpy((u8 *)val + 2, buf, len);
	val[0] = swab32(val[0]) |
		ump_compose(UMP_MSG_TYPE_SYSEX7, group, (status << 4) | len);
	val[1] = swab32(val[1]);
	data[0] = cpu_to_le32(val[0]);
	data[1] = cpu_to_le32(val[1]);
}

/* Convert sysex var event to UMP sysex7 packets and deliver them */
static int cvt_sysex_to_ump(struct snd_seq_client *dest,
			    struct snd_seq_client_port *dest_port,
			    struct snd_seq_event *event,
			    int atomic, int hop)
{
	struct snd_seq_event ev_cvt;
	__le32 *data = ev_cvt.data.ump.d;
	unsigned char status;
	u8 buf[8], *xbuf;
	int offset = 0;
	int len, err;

	if (!snd_seq_ev_is_variable(event))
		return 0;

	ev_cvt = *event;
	ev_cvt.type = SNDRV_SEQ_EVENT_UMP;
	ev_cvt.flags &= ~SNDRV_SEQ_EVENT_LENGTH_MASK;
	memset(&ev_cvt.data, 0, sizeof(ev_cvt.data));

	for (;;) {
		len = snd_seq_expand_var_event_at(event, sizeof(buf), buf,
						  offset);
		if (len <= 0)
			break;
		offset += len;
		status = UMP_SYSEX_STATUS_START;
		xbuf = buf;
		if (*xbuf == 0xf0) {
			xbuf++;
			len--;
			if (xbuf[len - 1] == 0xf7) {
				status = UMP_SYSEX_STATUS_SINGLE;
				len--;
			}
		} else {
			if (xbuf[len - 1] == 0xf7) {
				status = UMP_SYSEX_STATUS_END;
				len--;
			} else {
				status = UMP_SYSEX_STATUS_CONTINUE;
			}
		}
		fill_sysex7_ump(data, event->source.port, status, xbuf, len);
		err = __snd_seq_deliver_single_event(dest, dest_port,
						     &ev_cvt, atomic, hop);
		if (err < 0)
			return err;
	}
	return 0;
}

/* Convert to UMP packet and deliver */
int snd_seq_deliver_to_ump(struct snd_seq_client *dest,
			   struct snd_seq_client_port *dest_port,
			   struct snd_seq_event *event,
			   int atomic, int hop)
{
	if (event->type == SNDRV_SEQ_EVENT_SYSEX)
		return cvt_sysex_to_ump(dest, dest_port, event, atomic, hop);
	else if (snd_seq_client_is_midi2(dest))
		return cvt_to_ump_midi2(dest, dest_port, event, atomic, hop);
	else
		return cvt_to_ump_midi1(dest, dest_port, event, atomic, hop);
}
