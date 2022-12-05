/* SPDX-License-Identifier: GPL-2.0-or-later */
/*
 * Universal MIDI Packet (UMP) Support
 */
#ifndef __SOUND_UMP_H
#define __SOUND_UMP_H

#include <sound/rawmidi.h>

struct snd_ump_endpoint;
struct snd_ump_block;
struct snd_ump_ops;

struct snd_ump_endpoint {
	struct snd_rawmidi core;	/* raw UMP access */

	struct snd_ump_endpoint_info info;

	const struct snd_ump_ops *ops;	/* UMP ops set by the driver */
	struct snd_rawmidi_substream *substreams[2];	/* opened substreams */

	void *private_data;
	void (*private_free)(struct snd_ump_endpoint *ump);

	struct list_head fb_list;	/* list of snd_ump_block objects */

#if IS_ENABLED(CONFIG_SND_SEQUENCER)
	struct snd_seq_device *seq_dev;
	void *seq_client;
#endif
};

/* ops filled by UMP drivers */
struct snd_ump_ops {
	int (*open)(struct snd_ump_endpoint *ump, int dir);
	void (*close)(struct snd_ump_endpoint *ump, int dir);
	void (*trigger)(struct snd_ump_endpoint *ump, int dir, int up);
	void (*drain)(struct snd_ump_endpoint *ump, int dir);
};

struct snd_ump_block {
	struct snd_ump_block_info info;
	struct snd_ump_endpoint *ump;

	void *private_data;
	void (*private_free)(struct snd_ump_block *fb);

	struct list_head list;
};

#define rawmidi_to_ump(rmidi)	container_of(rmidi, struct snd_ump_endpoint, core)

int snd_ump_endpoint_new(struct snd_card *card, char *id, int device,
			 int output, int input,
			 struct snd_ump_endpoint **ump_ret);
int snd_ump_block_new(struct snd_ump_endpoint *ump, unsigned int blk,
		      unsigned int direction, unsigned int first_group,
		      unsigned int num_groups, struct snd_ump_block **fb_ret);
int snd_ump_receive(struct snd_ump_endpoint *ump,
		    const unsigned char *buffer, int count);
int snd_ump_transmit(struct snd_ump_endpoint *ump,
		     unsigned char *buffer, int count);

/*
 * Some definitions for UMP
 */

/* MIDI 2.0 Message Type */
enum {
	UMP_MSG_TYPE_UTILITY	= 0x00,
	UMP_MSG_TYPE_SYSTEM	= 0x01,
	UMP_MSG_TYPE_MIDI1	= 0x02,
	UMP_MSG_TYPE_SYSEX7	= 0x03,
	UMP_MSG_TYPE_MIDI2	= 0x04,
	UMP_MSG_TYPE_DATA8	= 0x05,
};

/* MIDI 2.0 SysEx / Data Running Status */
enum {
	UMP_SYSEX_STATUS_SINGLE		= 0,
	UMP_SYSEX_STATUS_START		= 1,
	UMP_SYSEX_STATUS_CONTINUE	= 2,
	UMP_SYSEX_STATUS_END		= 3,
};

/*
 * Helpers for retrieving / filling bits from UMP
 */
static inline unsigned char ump_message_type(u32 data)
{
	return data >> 28;
}

static inline unsigned char ump_message_group(u32 data)
{
	return (data >> 24) & 0x0f;
}

static inline unsigned char ump_message_status_code(u32 data)
{
	return (data >> 16) & 0xf0;
}

static inline unsigned char ump_message_channel(u32 data)
{
	return (data >> 16) & 0x0f;
}

static inline u32 ump_compose(unsigned char type, unsigned char group,
			      unsigned char status)
{
	return ((u32)type << 28) | ((u32)group << 24) | ((u32)status << 16);
}

static inline unsigned char ump_sysex_message_status(u32 data)
{
	return (data >> 20) & 0xf;
}

static inline unsigned char ump_sysex_message_length(u32 data)
{
	return (data >> 16) & 0xf;
}

#endif /* __SOUND_UMP_H */
