#![feature(int_roundings)]

mod structs;
use crate::structs::{BlockGroupDescriptor, DirectoryEntry, Inode, Superblock};
use null_terminated::NulStr;
use rustyline::{DefaultEditor, Result};
use std::fmt;
use std::fs; // for `cat` function, reading file
use std::mem;
use uuid::Uuid;
use zerocopy::ByteSlice;

#[repr(C)]
#[derive(Debug)]
pub struct Ext2 {
    pub superblock: &'static Superblock,
    pub block_groups: &'static [BlockGroupDescriptor],
    pub blocks: Vec<&'static [u8]>,
    pub block_size: usize,
    pub uuid: Uuid,
    pub block_offset: usize, // <- our "device data" actually starts at this index'th block of the device
                             // so we have to subtract this number before indexing blocks[]
}

const EXT2_MAGIC: u16 = 0xef53;
const EXT2_START_OF_SUPERBLOCK: usize = 1024;
const EXT2_END_OF_SUPERBLOCK: usize = 2048;

impl Ext2 {
    pub fn new<B: ByteSlice + std::fmt::Debug>(device_bytes: B, start_addr: usize) -> Ext2 {
        // https://wiki.osdev.org/Ext2#Superblock
        // parse into Ext2 struct - without copying

        // the superblock goes from bytes 1024 -> 2047
        let header_body_bytes = device_bytes.split_at(EXT2_END_OF_SUPERBLOCK);

        let superblock = unsafe {
            &*(header_body_bytes
                .0
                .split_at(EXT2_START_OF_SUPERBLOCK)
                .1
                .as_ptr() as *const Superblock)
        };
        assert_eq!(superblock.magic, EXT2_MAGIC);
        // at this point, we strongly suspect these bytes are indeed an ext2 filesystem

        println!("superblock:\n{:?}", superblock);
        println!("size of Inode struct: {}", mem::size_of::<Inode>());

        let block_group_count = superblock
            .blocks_count
            .div_ceil(superblock.blocks_per_group) as usize;

        let block_size: usize = 1024 << superblock.log_block_size;
        println!(
            "there are {} block groups and block_size = {}",
            block_group_count, block_size
        );
        let block_groups_rest_bytes = header_body_bytes.1.split_at(block_size);

        let block_groups = unsafe {
            std::slice::from_raw_parts(
                block_groups_rest_bytes.0.as_ptr() as *const BlockGroupDescriptor,
                block_group_count,
            )
        };

        println!("block group 0: {:?}", block_groups[0]);

        let blocks = unsafe {
            std::slice::from_raw_parts(
                block_groups_rest_bytes.1.as_ptr() as *const u8,
                // would rather use: device_bytes.as_ptr(),
                superblock.blocks_count as usize * block_size,
            )
        }
        .chunks(block_size)
        .collect::<Vec<_>>();

        let offset_bytes = (blocks[0].as_ptr() as usize) - start_addr;
        let block_offset = offset_bytes / block_size;
        let uuid = Uuid::from_bytes(superblock.fs_id);
        Ext2 {
            superblock,
            block_groups,
            blocks,
            block_size,
            uuid,
            block_offset,
        }
    }

    // given a (1-indexed) inode number, return that #'s inode structure
    pub fn get_inode(&self, inode: usize) -> &Inode {
        let group: usize = (inode - 1) / self.superblock.inodes_per_group as usize;
        let index: usize = (inode - 1) % self.superblock.inodes_per_group as usize;

        // println!("in get_inode, inode num = {}, index = {}, group = {}", inode, index, group);
        let inode_table_block =
            (self.block_groups[group].inode_table_block) as usize - self.block_offset;
        // println!("in get_inode, block number of inode table {}", inode_table_block);
        let inode_table = unsafe {
            std::slice::from_raw_parts(
                self.blocks[inode_table_block].as_ptr() as *const Inode,
                self.superblock.inodes_per_group as usize,
            )
        };
        // probably want a Vec of BlockGroups in our Ext structure so we don't have to slice each time,
        // but this works for now.
        // println!("{:?}", inode_table);
        &inode_table[index]
    }

    pub fn read_dir_inode(
        &self,
        inode: usize,
    ) -> std::io::Result<Vec<(usize, &NulStr, &structs::TypeIndicator)>> {
        let mut ret = Vec::new();
        let root = self.get_inode(inode);
        // println!("in read_dir_inode, #{} : {:?}", inode, root);
        // println!("following direct pointer to data block: {}", root.direct_pointer[0]);
        let entry_ptr = self.blocks[root.direct_pointer[0] as usize - self.block_offset].as_ptr();
        let mut byte_offset: isize = 0;
        while byte_offset < root.size_low as isize {
            // <- todo, support large directories
            let directory = unsafe { &*(entry_ptr.offset(byte_offset) as *const DirectoryEntry) };
            // println!("{:?}", directory);
            byte_offset += directory.entry_size as isize;
            ret.push((
                directory.inode as usize,
                &directory.name,
                &directory.type_indicator, // Store TypeIndicator for each inode as well
            ));
        }
        Ok(ret)
    }
}

impl fmt::Debug for Inode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.size_low == 0 && self.size_high == 0 {
            f.debug_struct("").finish()
        } else {
            f.debug_struct("Inode")
                .field("type_perm", &self.type_perm)
                .field("size_low", &self.size_low)
                .field("direct_pointers", &self.direct_pointer)
                .field("indirect_pointer", &self.indirect_pointer)
                .finish()
        }
    }
}

fn main() -> Result<()> {
    let disk = include_bytes!("../myfs.ext2");
    let start_addr: usize = disk.as_ptr() as usize;
    let ext2 = Ext2::new(&disk[..], start_addr);

    let mut current_working_inode: usize = 2;

    let mut rl = DefaultEditor::new()?;
    loop {
        // fetch the children of the current working directory
        let dirs = match ext2.read_dir_inode(current_working_inode) {
            Ok(dir_listing) => dir_listing,
            Err(_) => {
                println!("unable to read cwd");
                break;
            }
        };

        let buffer = rl.readline(":> ");
        if let Ok(line) = buffer {
            if line.starts_with("ls") {
                // `ls` prints our cwd's children
                // TODO: support arguments to ls (print that directory's children instead)
                for dir in &dirs {
                    print!("{}\t", dir.1);
                }
                println!();
            } else if line.starts_with("cd") {
                // `cd` with no arguments, cd goes back to root
                // `cd dir_name` moves cwd to that directory
                let elts: Vec<&str> = line.split(' ').collect();
                if elts.len() == 1 {
                    current_working_inode = 2;
                } else {
                    // TODO: if the argument is a path, follow the path
                    // e.g., cd dir_1/dir_2 should move you down 2 directories
                    // deeper into dir_2
                    let to_dir = elts[1];
                    let mut found = false;
                    for dir in &dirs {
                        if dir.1.to_string().eq(to_dir) {
                            found = true;
                            // TODO: maybe don't just assume this is a directory
                            match dir.2 {
                                structs::TypeIndicator::Directory => {
                                    current_working_inode = dir.0;
                                }
                                _ => {
                                    println!("can only cd into a directory");
                                }
                            }
                        }
                    }
                    if !found {
                        println!("unable to locate {}, cwd unchanged", to_dir);
                    }
                }
            } else if line.starts_with("mkdir") {
                // `mkdir childname`
                // create a directory with the given name, add a link to cwd
                // consider supporting `-p path/to_file` to create a path of directories
                println!("mkdir not yet implemented");
            } else if line.starts_with("cat") {
                // `cat filename`
                // print the contents of filename to stdout
                // if it's a directory, print a nice error
                let elts: Vec<&str> = line.split(' ').collect();
                let cat_file = elts[1];
                let mut found = false;
                for dir in &dirs {
                    if dir.1.to_string().eq(cat_file) {
                        found = true;
                        // Use iNode to read data of file
                        let contents = match ext2.read_dir_inode(dir.0) {
                            Ok(read) => read,
                            Err(_) => {
                                println!("unable to read file");
                                break;
                            }
                        };
                        match dir.2 {
                            structs::TypeIndicator::Regular => {
                                println!("{}", contents[0].1);
                            }
                            _ => {
                                println!("can only print the contents of a file");
                            }
                        }
                    }
                }
                if !found {
                    println!("unable to locate {}", cat_file);
                }
            } else if line.starts_with("rm") {
                // `rm target`
                // unlink a file or empty directory
                println!("rm not yet implemented");
            } else if line.starts_with("mount") {
                // `mount host_filename mountpoint`
                // mount an ext2 filesystem over an existing empty directory
                println!("mount not yet implemented");
            } else if line.starts_with("link") {
                // `link arg_1 arg_2`
                // create a hard link from arg_1 to arg_2
                // consider what to do if arg2 does- or does-not end in "/"
                // and/or if arg2 is an existing directory name
                println!("link not yet implemented");
            } else if line.starts_with("quit") || line.starts_with("exit") {
                break;
            }
        } else {
            println!("bye!");
            break;
        }
    }
    Ok(())
}
