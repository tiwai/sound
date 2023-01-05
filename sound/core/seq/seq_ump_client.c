// SPDX-License-Identifier: GPL-2.0-or-later
/* ALSA sequencer binding for UMP device */

#include <linux/init.h>
#include <linux/slab.h>
#include <linux/errno.h>
#include <linux/mutex.h>
#include <linux/string.h>
#include <linux/module.h>
#include <sound/core.h>
#include <sound/ump.h>
#include <sound/seq_kernel.h>
#include <sound/seq_device.h>
#include "seq_clientmgr.h"

struct seq_ump_client;
struct seq_ump_group;

enum { STR_IN, STR_OUT };

/* object per UMP group; corresponding to a sequencer port */
struct seq_ump_group {
	int group;			/* group index (0-based) */
	unsigned int dir_bits;		/* directions */
	bool active;			/* activeness */
	char name[64];			/* seq port name */
};

/* context for UMP input parsing, per EP */
struct seq_ump_input_buffer {
	unsigned char len;		/* total length in words */
	unsigned char pending;		/* pending words */
	unsigned char type;		/* parsed UMP packet type */
	unsigned char group;		/* parsed UMP packet group */
	__le32 buf[4];			/* incoming UMP packet */
};

/* sequencer client, per UMP EP (rawmidi) */
struct seq_ump_client {
	struct snd_ump_endpoint *ump;	/* assigned endpoint */
	int seq_client;			/* sequencer client id */
	int opened[2];			/* current opens for each direction */
	struct mutex open_mutex;
	struct seq_ump_input_buffer input; /* input parser context */
	struct snd_rawmidi_file rfile[2]; /* rawmidi for each dir */
	struct seq_ump_group groups[SNDRV_UMP_MAX_GROUPS]; /* table of groups */
	void *ump_info[SNDRV_UMP_MAX_BLOCKS + 1]; /* shadow of seq client ump_info */
};

#define is_groupless_msg(type) \
	((type) == UMP_MSG_TYPE_UTILITY || (type) == UMP_MSG_TYPE_UMP_STREAM)

/* number of 32bit words for each UMP message type */
static unsigned char ump_packet_words[0x10] = {
	1, 1, 1, 2, 2, 4, 1, 1, 2, 2, 2, 3, 3, 4, 4, 4
};

/* encode the input UMP packet to a sequencer event;
 * return true if the encoding finished for an event
 */
static bool seq_ump_encode_event(struct seq_ump_input_buffer *input,
				 struct snd_seq_event *ev, u32 val)
{
	int len;

	if (!input->pending) {
		input->pending = ump_packet_words[ump_message_type(val)];
		input->type = ump_message_type(val);
		input->group = ump_message_group(val);
		/* broadcast groupless messages */
		if (is_groupless_msg(input->type))
			input->group = SNDRV_UMP_MAX_GROUPS;
	}

	input->buf[input->len++] = cpu_to_le32(val);
	input->pending--;
	if (input->pending)
		return false;

	len = input->len << 2;
	input->len = 0;
	memset(ev, 0, sizeof(*ev));
	if (len <= 12) {
		ev->type = SNDRV_SEQ_EVENT_UMP;
		ev->source.port = input->group;
		memcpy(ev->data.ump.d, input->buf, len);
		return true;
	}

	/* TODO: concatenate multiple packets */
	ev->type = SNDRV_SEQ_EVENT_UMP_VAR;
	ev->flags = SNDRV_SEQ_EVENT_LENGTH_VARIABLE;
	ev->source.port = input->group;
	ev->data.ext.len = len;
	ev->data.ext.ptr = input->buf;
	return true;
}

/* process the incoming rawmidi stream */
static void seq_ump_input_event(struct snd_rawmidi_substream *substream)
{
	struct snd_rawmidi_runtime *runtime = substream->runtime;
	struct seq_ump_client *client = runtime->private_data;
	struct snd_seq_event ev;
	__le32 rawval;
	u32 val;

	while (runtime->avail > 0) {
		if (snd_rawmidi_kernel_read(substream, (unsigned char *)&rawval, 4) != 4)
			break;
		/* we assume always a LE stream */
		val = le32_to_cpu(rawval);
		if (!seq_ump_encode_event(&client->input, &ev, val))
			continue;

		ev.dest.client = SNDRV_SEQ_ADDRESS_SUBSCRIBERS;
		snd_seq_kernel_client_dispatch(client->seq_client,
					       &ev, true, 0);
	}
}

#define ump_get_type(data)	ump_message_type(cpu_to_le32(data))
#define ump_get_group(data)	ump_message_group(cpu_to_le32(data))

/* rewrite the group to the destination port and deliver */
static void write_packet_to_group(struct snd_rawmidi_substream *substream,
				  u8 group, __le32 *data, int len)
{
	*data &= cpu_to_le32(~(0xfU << 24));
	*data |= cpu_to_le32(group << 24);
	snd_rawmidi_kernel_write(substream, (unsigned char *)data, len);
}

/* process an input sequencer event; only deal with UMP types */
static int seq_ump_process_event(struct snd_seq_event *ev, int direct,
				 void *private_data, int atomic, int hop)
{
	struct seq_ump_client *client = private_data;
	struct snd_rawmidi_substream *substream;
	unsigned char type;
	unsigned char group;
	u8 *data;
	int len;
	union {
		u8 d8[16];
		__le32 d32[4];
	} packs;

	substream = client->rfile[STR_OUT].output;
	if (!substream)
		return -ENODEV;
	if (ev->type == SNDRV_SEQ_EVENT_UMP) {
		type = ump_get_type(ev->data.ump.d[0]);
		group = ump_get_group(ev->data.ump.d[0]);
		len = ump_packet_words[type];
		if (len >= 4)
			return 0; // invalid - skip
		len <<= 2;
		data = ev->data.raw8.d;
	} else {
		if (ev->type != SNDRV_SEQ_EVENT_UMP_VAR ||
		    !snd_seq_ev_is_variable(ev))
			return 0;

		len = snd_seq_expand_var_event(ev, 16, packs.d8, true, 0);
		if (len <= 0 || len % 8)
			return 0; // invalid - skip
		type = ump_get_type(packs.d32[0]);
		group = ump_get_group(packs.d32[0]);
		data = packs.d8;
	}

	if (ev->dest.port == SNDRV_UMP_MAX_GROUPS /* broadcast port */ ||
	    is_groupless_msg(type) ||
	    group == ev->dest.port) {
		/* copy as is */
		snd_rawmidi_kernel_write(substream, data, len);
		return 0;
	}

	/* copy with group conversion */
	if (data != packs.d8)
		memcpy(packs.d8, data, len);
	write_packet_to_group(substream, ev->dest.port, packs.d32, len);
	return 0;
}

static unsigned int get_rawmidi_flag(int dir)
{
	if (dir == STR_IN)
		return SNDRV_RAWMIDI_LFLG_INPUT;
	else
		return SNDRV_RAWMIDI_LFLG_OUTPUT;
}

/* open the rawmidi */
static int seq_ump_client_open(struct seq_ump_client *client, int dir)
{
	struct snd_rawmidi_runtime *runtime;
	int err = 0;

	mutex_lock(&client->open_mutex);
	if (!client->opened[dir]) {
		err = snd_rawmidi_kernel_open(&client->ump->core, 0,
					      get_rawmidi_flag(dir),
					      &client->rfile[dir]);
		if (err < 0)
			goto unlock;
		if (dir == STR_IN) {
			client->input.len = client->input.pending = 0;
			runtime = client->rfile[STR_IN].input->runtime;
			runtime->private_data = client;
			runtime->event = seq_ump_input_event;
			/* trigger once for starting reading */
			snd_rawmidi_kernel_read(client->rfile[STR_IN].input, NULL, 0);
		}
	}
	client->opened[dir]++;
 unlock:
	mutex_unlock(&client->open_mutex);
	return err;
}

/* close the rawmidi */
static int seq_ump_client_close(struct seq_ump_client *client, int dir)
{
	mutex_lock(&client->open_mutex);
	if (client->opened[dir] && !--client->opened[dir])
		snd_rawmidi_kernel_release(&client->rfile[dir]);
	mutex_unlock(&client->open_mutex);
	return 0;
}

/* sequencer subscription ops for each client */
static int seq_ump_subscribe(void *pdata, struct snd_seq_port_subscribe *info)
{
	struct seq_ump_client *client = pdata;

	return seq_ump_client_open(client, STR_IN);
}

static int seq_ump_unsubscribe(void *pdata, struct snd_seq_port_subscribe *info)
{
	struct seq_ump_client *client = pdata;

	return seq_ump_client_close(client, STR_IN);
}

static int seq_ump_use(void *pdata, struct snd_seq_port_subscribe *info)
{
	struct seq_ump_client *client = pdata;

	return seq_ump_client_open(client, STR_OUT);
}

static int seq_ump_unuse(void *pdata, struct snd_seq_port_subscribe *info)
{
	struct seq_ump_client *client = pdata;

	return seq_ump_client_close(client, STR_OUT);
}

/* fill port_info from the given UMP EP and group info */
static void fill_port_info(struct snd_seq_port_info *port,
			   struct seq_ump_client *client,
			   struct seq_ump_group *group)
{
	port->addr.client = client->seq_client;
	port->addr.port = group->group;
	port->capability = 0;
	if (group->dir_bits & (1 << STR_OUT)) {
		port->capability |= SNDRV_SEQ_PORT_CAP_WRITE |
			SNDRV_SEQ_PORT_CAP_SYNC_WRITE |
			SNDRV_SEQ_PORT_CAP_SUBS_WRITE;
	}
	if (group->dir_bits & (1 << STR_IN)) {
		port->capability |= SNDRV_SEQ_PORT_CAP_READ |
			SNDRV_SEQ_PORT_CAP_SYNC_READ |
			SNDRV_SEQ_PORT_CAP_SUBS_READ;
		if (group->dir_bits & (1 << STR_OUT))
			port->capability |= SNDRV_SEQ_PORT_CAP_DUPLEX;
	}
	if (!group->active)
		port->capability |= SNDRV_SEQ_PORT_CAP_DISABLED;
	port->type = SNDRV_SEQ_PORT_TYPE_MIDI_UMP |
		SNDRV_SEQ_PORT_TYPE_HARDWARE |
		SNDRV_SEQ_PORT_TYPE_PORT;
	port->midi_channels = 16;
	if (*group->name)
		strscpy(port->name, group->name, sizeof(port->name));
	else
		sprintf(port->name, "Group %d", group->group);
}

/* create a new sequencer port per UMP group */
static int seq_ump_group_init(struct seq_ump_client *client, int group_index)
{
	struct seq_ump_group *group = &client->groups[group_index];
	struct snd_seq_port_info *port;
	struct snd_seq_port_callback pcallbacks;
	int err;

	port = kzalloc(sizeof(*port), GFP_KERNEL);
	if (!port) {
		err = -ENOMEM;
		goto error;
	}

	fill_port_info(port, client, group);
	port->flags = SNDRV_SEQ_PORT_FLG_GIVEN_PORT;
	memset(&pcallbacks, 0, sizeof(pcallbacks));
	pcallbacks.owner = THIS_MODULE;
	pcallbacks.private_data = client;
	pcallbacks.subscribe = seq_ump_subscribe;
	pcallbacks.unsubscribe = seq_ump_unsubscribe;
	pcallbacks.use = seq_ump_use;
	pcallbacks.unuse = seq_ump_unuse;
	pcallbacks.event_input = seq_ump_process_event;
	port->kernel = &pcallbacks;
	err = snd_seq_kernel_client_ctl(client->seq_client,
					SNDRV_SEQ_IOCTL_CREATE_PORT,
					port);
 error:
	kfree(port);
	return err;
}

/* update dir_bits and active flag for all groups in the client */
static void update_group_attrs(struct seq_ump_client *client)
{
	struct snd_ump_block *fb;
	struct seq_ump_group *group;
	int i;

	for (i = 0; i < SNDRV_UMP_MAX_GROUPS; i++) {
		group = &client->groups[i];
		*group->name = 0;
		group->dir_bits = 0;
		group->active = 0;
		group->group = i;
	}

	list_for_each_entry(fb, &client->ump->fb_list, list) {
		if (fb->info.first_group < 0 ||
		    fb->info.first_group + fb->info.num_groups > SNDRV_UMP_MAX_GROUPS)
			break;
		group = &client->groups[fb->info.first_group];
		for (i = 0; i < fb->info.num_groups; i++, group++) {
			if (fb->info.active)
				group->active = 1;
			switch (fb->info.direction) {
			case SNDRV_UMP_DIR_INPUT:
				group->dir_bits |= (1 << STR_IN);
				break;
			case SNDRV_UMP_DIR_OUTPUT:
				group->dir_bits |= (1 << STR_OUT);
				break;
			case SNDRV_UMP_DIR_BIDIRECTION:
				group->dir_bits |= (1 << STR_OUT) | (1 << STR_IN);
				break;
			}
			if (!*group->name && *fb->info.name)
				strscpy(group->name, fb->info.name,
					sizeof(group->name));
		}
	}
}

/* create a client-broadcast port (that corresponds to a UMP EP) */
static int create_broadcast_port(struct seq_ump_client *client)
{
	struct snd_seq_port_info *port;
	struct snd_seq_port_callback pcallbacks;
	int err;

	if (!(client->ump->core.info_flags & SNDRV_RAWMIDI_INFO_INPUT))
		return 0;

	port = kzalloc(sizeof(*port), GFP_KERNEL);
	if (!port)
		return -ENOMEM;

	port->addr.client = client->seq_client;
	port->addr.port = SNDRV_UMP_MAX_GROUPS;
	port->flags = SNDRV_SEQ_PORT_FLG_GIVEN_PORT;
	port->capability = SNDRV_SEQ_PORT_CAP_READ |
		SNDRV_SEQ_PORT_CAP_SYNC_READ |
		SNDRV_SEQ_PORT_CAP_SUBS_READ |
		SNDRV_SEQ_PORT_CAP_BROADCAST;
	if (client->ump->core.info_flags & SNDRV_RAWMIDI_INFO_OUTPUT)
		port->capability |= SNDRV_SEQ_PORT_CAP_WRITE |
			SNDRV_SEQ_PORT_CAP_SYNC_WRITE |
			SNDRV_SEQ_PORT_CAP_SUBS_WRITE |
			SNDRV_SEQ_PORT_CAP_DUPLEX;
	port->type = SNDRV_SEQ_PORT_TYPE_MIDI_UMP |
		SNDRV_SEQ_PORT_TYPE_HARDWARE |
		SNDRV_SEQ_PORT_TYPE_PORT;
	port->midi_channels = 16;
	strcpy(port->name, "UMP Endpoint");
	memset(&pcallbacks, 0, sizeof(pcallbacks));
	pcallbacks.owner = THIS_MODULE;
	pcallbacks.private_data = client;
	pcallbacks.subscribe = seq_ump_subscribe;
	pcallbacks.unsubscribe = seq_ump_unsubscribe;
	if (client->ump->core.info_flags & SNDRV_RAWMIDI_INFO_OUTPUT) {
		pcallbacks.use = seq_ump_use;
		pcallbacks.unuse = seq_ump_unuse;
		pcallbacks.event_input = seq_ump_process_event;
	}
	port->kernel = &pcallbacks;
	err = snd_seq_kernel_client_ctl(client->seq_client,
					SNDRV_SEQ_IOCTL_CREATE_PORT,
					port);
	kfree(port);
	return err;
}

/* release the client resources */
static void seq_ump_client_free(struct seq_ump_client *client)
{
	if (client->seq_client >= 0)
		snd_seq_delete_kernel_client(client->seq_client);

	client->ump->seq_ops = NULL;
	client->ump->seq_client = NULL;

	kfree(client);
}

/* update the MIDI version for the given client */
static void setup_client_midi_version(struct seq_ump_client *client)
{
	struct snd_seq_client *cptr;

	cptr = snd_seq_kernel_client_get(client->seq_client);
	if (!cptr)
		return;
	if (client->ump->info.protocol & SNDRV_UMP_EP_INFO_PROTO_MIDI2)
		cptr->midi_version = SNDRV_SEQ_CLIENT_UMP_MIDI_2_0;
	else
		cptr->midi_version = SNDRV_SEQ_CLIENT_UMP_MIDI_1_0;
	snd_seq_kernel_client_put(cptr);
}

/* UMP sequencer ops for switching protocol */
static int seq_ump_switch_protocol(struct snd_ump_endpoint *ump)
{
	if (!ump->seq_client)
		return -ENODEV;
	setup_client_midi_version(ump->seq_client);
	return 0;
}

static const struct snd_seq_ump_ops seq_ump_ops = {
	.switch_protocol = seq_ump_switch_protocol,
};

/* create a sequencer client and ports for the given UMP endpoint */
static int snd_seq_ump_probe(struct device *_dev)
{
	struct snd_seq_device *dev = to_seq_dev(_dev);
	struct snd_ump_endpoint *ump = dev->private_data;
	struct snd_card *card = dev->card;
	struct seq_ump_client *client;
	struct snd_ump_block *fb;
	struct snd_seq_client *cptr;
	int p, err;

	client = kzalloc(sizeof(*client), GFP_KERNEL);
	if (!client)
		return -ENOMEM;

	mutex_init(&client->open_mutex);
	client->ump = ump;

	client->seq_client =
		snd_seq_create_kernel_client(card, ump->core.device,
					     ump->core.name);
	if (client->seq_client < 0) {
		err = client->seq_client;
		goto error;
	}

	client->ump_info[0] = &ump->info;
	list_for_each_entry(fb, &ump->fb_list, list)
		client->ump_info[fb->info.block_id + 1] = &fb->info;

	setup_client_midi_version(client);
	update_group_attrs(client);

	for (p = 0; p < SNDRV_UMP_MAX_GROUPS; p++) {
		err = seq_ump_group_init(client, p);
		if (err < 0)
			goto error;
	}

	err = create_broadcast_port(client);
	if (err < 0)
		goto error;

	cptr = snd_seq_kernel_client_get(client->seq_client);
	if (!cptr) {
		err = -EINVAL;
		goto error;
	}
	cptr->ump_info = client->ump_info;
	snd_seq_kernel_client_put(cptr);

	ump->seq_ops = &seq_ump_ops;
	ump->seq_client = client;
	return 0;

 error:
	seq_ump_client_free(client);
	return err;
}

/* remove a sequencer client */
static int snd_seq_ump_remove(struct device *_dev)
{
	struct snd_seq_device *dev = to_seq_dev(_dev);
	struct snd_ump_endpoint *ump = dev->private_data;

	if (ump->seq_client)
		seq_ump_client_free(ump->seq_client);
	return 0;
}

static struct snd_seq_driver seq_ump_driver = {
	.driver = {
		.name = KBUILD_MODNAME,
		.probe = snd_seq_ump_probe,
		.remove = snd_seq_ump_remove,
	},
	.id = SNDRV_SEQ_DEV_ID_UMP,
	.argsize = 0,
};

module_snd_seq_driver(seq_ump_driver);

MODULE_DESCRIPTION("ALSA sequencer client for UMP rawmidi");
MODULE_LICENSE("GPL");
