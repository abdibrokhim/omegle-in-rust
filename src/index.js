const express = require('express');
const http = require('http');
const { Server } = require('socket.io');
const cors = require('cors');
const crypto = require('crypto');

const app = express();

// Enable CORS
app.use(cors());

const server = http.createServer(app);
const io = new Server(server, {
  cors: {
    origin: "https://yaps.chat",
    methods: ["GET", "POST"]
  }
});

// Users waiting to be matched
const waitingUsers = {
  male: [], // Users looking for male partners
  female: [], // Users looking for female partners
  group: [] // Users looking for group chats
};

// Active connections
const activeConnections = new Map();

// User data store
const users = new Map();

// Group chats
const groups = new Map();

// Debug logging
const logLevels = {
  DEBUG: 'DEBUG',
  INFO: 'INFO',
  WARN: 'WARN',
  ERROR: 'ERROR'
};

function log(level, component, message, data = null) {
  const timestamp = new Date().toISOString();
  const entry = {
    timestamp,
    level,
    component,
    message,
    data
  };
  
  // console.log(`[${timestamp}] [${level}] [${component}]: ${message}`);
  // if (data) {
  //   console.log('Data:', JSON.stringify(data, null, 2));
  // }
  
  return entry;
}

// Generate a random 6-character alphanumeric code
function generateGroupCode() {
  return crypto.randomBytes(3).toString('hex').toUpperCase();
}

// Print all active groups every 10 seconds for debugging
setInterval(() => {
  log(logLevels.DEBUG, 'GroupMonitor', `Active groups: ${groups.size}`);
  if (groups.size > 0) {
    groups.forEach((group, code) => {
      log(logLevels.DEBUG, 'GroupMonitor', `Group ${code}: ${group.members.length} members`, {
        code,
        members: group.usernames,
        memberIds: group.members
      });
    });
  }
}, 10000);

io.on('connection', (socket) => {
  log(logLevels.INFO, 'Connection', `User connected: ${socket.id}`);
  
  // Handle user joining the chat
  socket.on('join_chat', (userProfile) => {
    const { userId, username, preference, gender, roomType, groupCode, groupJoinMethod } = userProfile;
    
    log(logLevels.INFO, 'JoinChat', `User ${socket.id} joining chat`, { 
      userId, username, preference, gender, roomType, groupJoinMethod, 
      customGroupCode: groupCode 
    });
    
    // Save user data
    users.set(socket.id, { 
      userId, 
      username: username || `User-${userId.substring(0, 5)}`,
      socketId: socket.id,
      gender, 
      preference,
      roomType: roomType || 'couple',
      partnerId: null,
      groupId: null
    });
    
    // Handle based on room type
    if (roomType === 'group') {
      log(logLevels.INFO, 'JoinChat', `User ${socket.id} joining a group chat with method: ${groupJoinMethod}`);
      handleGroupChatJoin(socket.id, groupJoinMethod, groupCode);
    } else {
      // Find a match for couple chat
      log(logLevels.INFO, 'JoinChat', `User ${socket.id} looking for a couple chat`);
      findMatch(socket.id);
    }
  });
  
  // Handle chat messages
  socket.on('send_message', (data) => {
    const { message, isGroupChat, groupCode } = data;
    const user = users.get(socket.id);
    
    if (!user) {
      log(logLevels.WARN, 'SendMessage', `User ${socket.id} not found in users map`);
      return;
    }
    
    // Validate that message contains required encryption fields if it's an object
    if (typeof message === 'object' && (!message.encrypted || !message.nonce)) {
      log(logLevels.WARN, 'SendMessage', `Invalid encrypted message format from ${socket.id}`, { message });
      return;
    }
    
    if (isGroupChat) {
      // Send to group
      log(logLevels.INFO, 'SendMessage', `User ${socket.id} sending encrypted message to group ${groupCode || user.groupId}`);
      sendMessageToGroup(socket.id, message, groupCode || user.groupId);
    } else {
      // Send to partner
      if (user.partnerId) {
        log(logLevels.INFO, 'SendMessage', `User ${socket.id} sending encrypted message to partner ${user.partnerId}`);
        const partnerSocket = io.sockets.sockets.get(user.partnerId);
        if (partnerSocket) {
          partnerSocket.emit('receive_message', {
            message,
            sender: user.username
          });
        } else {
          log(logLevels.WARN, 'SendMessage', `Partner socket ${user.partnerId} not found`);
        }
      } else {
        log(logLevels.WARN, 'SendMessage', `User ${socket.id} has no partner to send message to`);
      }
    }
  });
  
  // Handle typing indicators
  socket.on('typing_start', (data) => {
    const { isGroupChat, groupCode } = data;
    const user = users.get(socket.id);
    
    if (!user) return;
    
    if (isGroupChat) {
      // Notify group members
      const group = findGroupByCode(groupCode || user.groupId);
      if (group) {
        group.members.forEach(memberId => {
          if (memberId !== socket.id) {
            const memberSocket = io.sockets.sockets.get(memberId);
            if (memberSocket) {
              memberSocket.emit('typing_started', { username: user.username });
            }
          }
        });
      } else {
        log(logLevels.WARN, 'TypingStart', `Group ${groupCode || user.groupId} not found`);
      }
    } else {
      // Notify partner
      if (user.partnerId) {
        const partnerSocket = io.sockets.sockets.get(user.partnerId);
        if (partnerSocket) {
          partnerSocket.emit('typing_started');
        }
      }
    }
  });
  
  socket.on('typing_stop', (data) => {
    const { isGroupChat, groupCode } = data;
    const user = users.get(socket.id);
    
    if (!user) return;
    
    if (isGroupChat) {
      // Notify group members
      const group = findGroupByCode(groupCode || user.groupId);
      if (group) {
        group.members.forEach(memberId => {
          if (memberId !== socket.id) {
            const memberSocket = io.sockets.sockets.get(memberId);
            if (memberSocket) {
              memberSocket.emit('typing_stopped', { username: user.username });
            }
          }
        });
      }
    } else {
      // Notify partner
      if (user.partnerId) {
        const partnerSocket = io.sockets.sockets.get(user.partnerId);
        if (partnerSocket) {
          partnerSocket.emit('typing_stopped');
        }
      }
    }
  });
  
  // Handle user manually disconnecting from a chat
  socket.on('disconnect_chat', () => {
    log(logLevels.INFO, 'DisconnectChat', `User ${socket.id} manually disconnecting`);
    handleDisconnect(socket.id);
  });
  
  // Handle user disconnecting (closing browser, etc.)
  socket.on('disconnect', () => {
    log(logLevels.INFO, 'Disconnect', `User disconnected: ${socket.id}`);
    handleDisconnect(socket.id);
    removeFromWaitingList(socket.id);
    users.delete(socket.id);
  });
});

// Find a match for a user based on their preference in couple mode
function findMatch(socketId) {
  const user = users.get(socketId);
  if (!user) {
    log(logLevels.WARN, 'FindMatch', `User ${socketId} not found`);
    return;
  }
  
  log(logLevels.INFO, 'FindMatch', `Finding match for user ${socketId} with preference ${user.preference}`);
  
  // Determine which waiting list to check based on preference
  let matchPool = [];
  
  if (user.preference === 'male') {
    // Looking for males
    matchPool = waitingUsers.male.filter(id => {
      const potentialMatch = users.get(id);
      return potentialMatch && potentialMatch.gender === 'male';
    });
    log(logLevels.DEBUG, 'FindMatch', `Found ${matchPool.length} potential male matches`);
  } else if (user.preference === 'female') {
    // Looking for females
    matchPool = waitingUsers.female.filter(id => {
      const potentialMatch = users.get(id);
      return potentialMatch && potentialMatch.gender === 'female';
    });
    log(logLevels.DEBUG, 'FindMatch', `Found ${matchPool.length} potential female matches`);
  } else if (user.preference === 'group') {
    // Should not happen, handled separately
    log(logLevels.WARN, 'FindMatch', `Group preference should be handled by handleGroupChatJoin`);
    return;
  }
  
  if (matchPool.length > 0) {
    // Get a random user from the matching pool
    const randomIndex = Math.floor(Math.random() * matchPool.length);
    const partnerId = matchPool[randomIndex];
    
    log(logLevels.INFO, 'FindMatch', `Matched user ${socketId} with ${partnerId}`);
    
    // Connect the two users
    connectUsers(socketId, partnerId);
  } else {
    // No match found, add user to waiting list
    log(logLevels.INFO, 'FindMatch', `No match found for user ${socketId}, adding to waiting list`);
    
    if (user.preference === 'male') {
      waitingUsers.male.push(socketId);
    } else if (user.preference === 'female') {
      waitingUsers.female.push(socketId);
    } else if (user.preference === 'group') {
      waitingUsers.group.push(socketId);
    }
    
    // Notify user they're waiting for a match
    const socket = io.sockets.sockets.get(socketId);
    if (socket) {
      socket.emit('waiting_for_match');
    }
    
    log(logLevels.DEBUG, 'FindMatch', `Waiting users:`, waitingUsers);
  }
}

// Handle a user joining a group chat
function handleGroupChatJoin(socketId, joinMethod, customGroupCode) {
  const user = users.get(socketId);
  if (!user) {
    log(logLevels.WARN, 'GroupJoin', `User ${socketId} not found`);
    return;
  }
  
  log(logLevels.INFO, 'GroupJoin', `User ${socketId} joining group with method ${joinMethod}`, { customGroupCode });
  
  if (joinMethod === 'create') {
    // Create a new group
    log(logLevels.INFO, 'GroupJoin', `User ${socketId} creating a new group`);
    createNewGroup(socketId);
  } else if (joinMethod === 'join' && customGroupCode) {
    // Join an existing group by code
    log(logLevels.INFO, 'GroupJoin', `User ${socketId} joining group with code ${customGroupCode}`);
    joinGroupByCode(socketId, customGroupCode);
  } else if (joinMethod === 'random') {
    // Join a random existing group or create a new one
    log(logLevels.INFO, 'GroupJoin', `User ${socketId} joining random group`);
    joinRandomGroup(socketId);
  } else {
    log(logLevels.WARN, 'GroupJoin', `Invalid join method: ${joinMethod}`);
  }
}

// Create a new group chat
function createNewGroup(socketId) {
  const user = users.get(socketId);
  if (!user) {
    log(logLevels.WARN, 'CreateGroup', `User ${socketId} not found`);
    return;
  }
  
  const groupCode = generateGroupCode();
  const socket = io.sockets.sockets.get(socketId);
  
  log(logLevels.INFO, 'CreateGroup', `Creating new group ${groupCode} for user ${socketId}`);
  
  // Create new group
  groups.set(groupCode, {
    code: groupCode,
    members: [socketId],
    usernames: [user.username]
  });
  
  // Update user data
  user.groupId = groupCode;
  
  // Notify user
  if (socket) {
    socket.join(groupCode); // Join socket.io room
    socket.emit('chat_started', { groupCode });
    socket.emit('group_members_update', [user.username]);
    
    log(logLevels.INFO, 'CreateGroup', `User ${socketId} joined new group ${groupCode}`);
  } else {
    log(logLevels.WARN, 'CreateGroup', `Socket ${socketId} not found`);
  }
  
  // Log all groups
  log(logLevels.DEBUG, 'CreateGroup', `All groups:`, Array.from(groups.entries()));
}

// Join a group by code
function joinGroupByCode(socketId, groupCode) {
  const user = users.get(socketId);
  const socket = io.sockets.sockets.get(socketId);
  
  if (!user || !socket) {
    log(logLevels.WARN, 'JoinGroup', `User ${socketId} or socket not found`);
    return;
  }
  
  log(logLevels.INFO, 'JoinGroup', `User ${socketId} trying to join group ${groupCode}`);
  
  const group = findGroupByCode(groupCode);
  
  if (!group) {
    // Group not found
    log(logLevels.WARN, 'JoinGroup', `Group ${groupCode} not found`);
    socket.emit('group_not_found');
    return;
  }
  
  // Add user to group
  group.members.push(socketId);
  group.usernames.push(user.username);
  
  // Update user data
  user.groupId = groupCode;
  
  log(logLevels.INFO, 'JoinGroup', `User ${socketId} joined group ${groupCode}`, {
    members: group.members,
    usernames: group.usernames
  });
  
  // Notify all group members
  socket.join(groupCode); // Join socket.io room
  socket.emit('chat_started', { groupCode });
  io.to(groupCode).emit('group_members_update', group.usernames);
  
  // Notify others that a new user joined
  socket.to(groupCode).emit('user_joined_group', user.username);
}

// Join a random group
function joinRandomGroup(socketId) {
  const user = users.get(socketId);
  const socket = io.sockets.sockets.get(socketId);
  
  if (!user || !socket) {
    log(logLevels.WARN, 'JoinRandomGroup', `User ${socketId} or socket not found`);
    return;
  }
  
  // Find available groups with at least one member
  const availableGroups = Array.from(groups.values())
    .filter(group => group.members.length > 0);
  
  log(logLevels.INFO, 'JoinRandomGroup', `Found ${availableGroups.length} available groups for user ${socketId}`);
  log(logLevels.DEBUG, 'JoinRandomGroup', `Available groups:`, availableGroups);
  
  if (availableGroups.length === 0) {
    // No groups available, create a new one
    log(logLevels.INFO, 'JoinRandomGroup', `No groups available, creating a new one for user ${socketId}`);
    createNewGroup(socketId);
    return;
  }
  
  // Select a random group
  const randomIndex = Math.floor(Math.random() * availableGroups.length);
  const randomGroup = availableGroups[randomIndex];
  
  log(logLevels.INFO, 'JoinRandomGroup', `Selected random group ${randomGroup.code} for user ${socketId}`);
  
  // Join the group
  joinGroupByCode(socketId, randomGroup.code);
}

// Find a group by its code
function findGroupByCode(groupCode) {
  const group = groups.get(groupCode);
  if (!group) {
    log(logLevels.DEBUG, 'FindGroup', `Group ${groupCode} not found`);
  }
  return group;
}

// Send a message to a group
function sendMessageToGroup(socketId, message, groupCode) {
  const group = findGroupByCode(groupCode);
  const user = users.get(socketId);
  
  if (!group) {
    log(logLevels.WARN, 'SendMessageToGroup', `Group ${groupCode} not found`);
    return;
  }
  
  if (!user) {
    log(logLevels.WARN, 'SendMessageToGroup', `User ${socketId} not found`);
    return;
  }
  
  // Get the username for the sender
  const username = user.username || 'Anonymous';
  
  // Send to all members of the group except the sender
  group.members.forEach(memberId => {
    if (memberId !== socketId) {
      const memberSocket = io.sockets.sockets.get(memberId);
      if (memberSocket) {
        log(logLevels.DEBUG, 'SendMessageToGroup', `Sending encrypted message to group member ${memberId}`);
        memberSocket.emit('receive_message', {
          message,
          sender: username
        });
      }
    }
  });
}

// Connect two users for couple chatting
function connectUsers(user1Id, user2Id) {
  const user1 = users.get(user1Id);
  const user2 = users.get(user2Id);
  
  if (!user1 || !user2) {
    log(logLevels.WARN, 'ConnectUsers', `One of the users not found: ${user1Id}, ${user2Id}`);
    return;
  }
  
  log(logLevels.INFO, 'ConnectUsers', `Connecting users ${user1Id} and ${user2Id}`);
  
  // Update user records with partner info
  user1.partnerId = user2Id;
  user2.partnerId = user1Id;
  
  // Remove both users from waiting lists
  removeFromWaitingList(user1Id);
  removeFromWaitingList(user2Id);
  
  // Add to active connections
  activeConnections.set(user1Id, user2Id);
  activeConnections.set(user2Id, user1Id);
  
  // Notify both users that chat has started
  const socket1 = io.sockets.sockets.get(user1Id);
  const socket2 = io.sockets.sockets.get(user2Id);
  
  if (socket1) socket1.emit('chat_started');
  if (socket2) socket2.emit('chat_started');
  
  log(logLevels.INFO, 'ConnectUsers', `Users connected successfully: ${user1Id} and ${user2Id}`);
}

// Remove a user from all waiting lists
function removeFromWaitingList(socketId) {
  waitingUsers.male = waitingUsers.male.filter(id => id !== socketId);
  waitingUsers.female = waitingUsers.female.filter(id => id !== socketId);
  waitingUsers.group = waitingUsers.group.filter(id => id !== socketId);
  
  log(logLevels.DEBUG, 'RemoveFromWaitingList', `Removed user ${socketId} from all waiting lists`);
}

// Handle a user disconnecting
function handleDisconnect(socketId) {
  const user = users.get(socketId);
  if (!user) {
    log(logLevels.WARN, 'HandleDisconnect', `User ${socketId} not found`);
    return;
  }
  
  log(logLevels.INFO, 'HandleDisconnect', `Handling disconnect for user ${socketId}`, user);
  
  if (user.roomType === 'group' && user.groupId) {
    // Handle group chat disconnection
    const group = findGroupByCode(user.groupId);
    if (group) {
      log(logLevels.INFO, 'HandleDisconnect', `User ${socketId} disconnecting from group ${user.groupId}`);
      
      // Remove user from group
      group.members = group.members.filter(id => id !== socketId);
      group.usernames = group.usernames.filter(name => name !== user.username);
      
      log(logLevels.DEBUG, 'HandleDisconnect', `Updated group ${user.groupId}`, {
        members: group.members,
        usernames: group.usernames
      });
      
      if (group.members.length === 0) {
        // If group is empty, remove it
        log(logLevels.INFO, 'HandleDisconnect', `Group ${user.groupId} is empty, removing it`);
        groups.delete(user.groupId);
      } else {
        // Notify remaining members
        log(logLevels.INFO, 'HandleDisconnect', `Notifying remaining members in group ${user.groupId}`);
        io.to(user.groupId).emit('user_left_group', user.username);
        io.to(user.groupId).emit('group_members_update', group.usernames);
      }
      
      // Leave socket.io room
      const socket = io.sockets.sockets.get(socketId);
      if (socket) {
        socket.leave(user.groupId);
      }
    } else {
      log(logLevels.WARN, 'HandleDisconnect', `Group ${user.groupId} not found for user ${socketId}`);
    }
  } else {
    // Handle couple chat disconnection
    // Check if the user was in an active chat
    if (activeConnections.has(socketId)) {
      const partnerId = activeConnections.get(socketId);
      const partnerSocket = io.sockets.sockets.get(partnerId);
      
      log(logLevels.INFO, 'HandleDisconnect', `User ${socketId} disconnecting from couple chat with partner ${partnerId}`);
      
      // Notify the partner that the user has disconnected
      if (partnerSocket) {
        partnerSocket.emit('partner_disconnected');
      }
      
      // Update user data
      const partner = users.get(partnerId);
      if (partner) {
        partner.partnerId = null;
      }
      
      // Remove from active connections
      activeConnections.delete(socketId);
      activeConnections.delete(partnerId);
      
      log(logLevels.INFO, 'HandleDisconnect', `Removed users ${socketId} and ${partnerId} from active connections`);
    } else {
      log(logLevels.DEBUG, 'HandleDisconnect', `User ${socketId} was not in an active couple chat`);
    }
  }
}

// Start server
const PORT = process.env.PORT || 3001;
server.listen(PORT, () => {
  log(logLevels.INFO, 'Server', `Socket.io server running on port ${PORT}`);
});

// Basic route for checking server status
app.get('/', (req, res) => {
  res.send('Socket.io server for Random Tune Harmony chat is running');
});
