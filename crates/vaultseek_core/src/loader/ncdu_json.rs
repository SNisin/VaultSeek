use std::{error::Error, path::Path};

use serde::{Deserialize, Serialize, de};
use serde_json::{Deserializer, Value};

use crate::file_tree::FileTree;

type NcduTopLevel = (i32, i32, Value, NcduDirectory);
// [
//   <majorver>,
//   <minorver>,
//   <metadata>,
//   <directory>
// ]

fn one() -> u32 {
    1
}
#[derive(Deserialize, Serialize, Debug)]
struct NcduInfoBlock {
    name: String,
    asize: Option<i64>,
    dsize: Option<i64>,
    #[serde(default)]
    dev: u64, // device id
    #[serde(default)]
    ino: u64, //inode number
    #[serde(default)]
    hlnkc: bool, // true if nlink > 1
    #[serde(default)]
    read_error: bool,
    excluded: Option<String>,
    #[serde(default = "one")]
    nlink: u32, // number of links to inode
    #[serde(default)]
    notreg: bool, // true if not a regular file or directory. i.e. symlink, device file, FIFO, socket, etc.
    uid: Option<u32>,
    gid: Option<u32>,
    mode: Option<u16>,
    mtime: Option<u64>,
}

type NcduDirectory = Vec<NcduDirectoryEntry>;
#[derive(Deserialize, Serialize, Debug)]
#[serde(untagged)]
enum NcduDirectoryEntry {
    InfoBlock(NcduInfoBlock),
    Directory(NcduDirectory),
}

#[derive(Deserialize, Serialize, Debug)]
struct Record {
    #[serde(rename = "Filename")]
    filename: String,
    #[serde(rename = "Size")]
    size: Option<i64>,
    #[serde(rename = "Date Modified")]
    date_modified: Option<i64>,
    #[serde(rename = "Date Created")]
    date_created: Option<i64>,
    #[serde(rename = "Attributes")]
    attributes: u32,
}

fn get_date_modified_from_info(info: &NcduInfoBlock) -> Option<i64> {
    // convert to windows FILETIME (100-nanosecond intervals since January 1, 1601)
    if let Some(mtime) = info.mtime {
        let unix_epoch_start = 11644473600i64; // seconds between 1601 and 1970
        let filetime = (mtime as i64 + unix_epoch_start) * 10_000_000;
        Some(filetime)
    } else {
        None
    }
}

fn get_attributes(info: &NcduInfoBlock, isdir: bool, filename: &str) -> u32 {
    // From octal:
    // 0140000   socket
    // 0120000   symbolic link
    // 0100000   regular file
    // 0060000   block device
    // 0040000   directory
    // 0020000   character device
    // 0010000   FIFO

    let node_type = 0o170000 & info.mode.unwrap_or(0);

    // To:
    // 1: Read-only
    // 2: Hidden
    // 4: System
    // 16: Directory
    // 32: Archive
    // 128: Normal
    // 256: Temporary
    // 512: Sparse file
    // 1024: Reparse point
    // 2048: Compressed
    // 4096: Offline
    // 8192: Not content indexed
    // 16384: Encrypted

    let mut attributes = 0u32;
    if node_type == 0o40000 || isdir {
        attributes |= 16; // FILE_ATTRIBUTE_DIRECTORY
    }
    if node_type == 0o120000 {
        // symbolic link
        attributes |= 0x400; // FILE_ATTRIBUTE_REPARSE_POINT
    }
    if let Some(mode) = info.mode {
        if mode & 0o200 == 0 {
            attributes |= 1; // FILE_ATTRIBUTE_READONLY
        }
    }
    if filename.starts_with('.') {
        attributes |= 2; // FILE_ATTRIBUTE_HIDDEN
    }
    attributes
}

pub fn import_ncdu_json<P: AsRef<Path>>(filepath: P) -> Result<FileTree, Box<dyn Error>> {
    let file_list_reader = std::fs::File::open(filepath)?;

    let data: NcduTopLevel = serde_json::from_reader(&file_list_reader)?;

    println!("Major: {}, Minor: {}, Metadata: {}", data.0, data.1, data.2);

    // Estimate the number of records in the file
    let file_size = file_list_reader.metadata()?.len();
    // Assuming an average record size of 100 bytes, adjust as necessary
    let estimated_records = (file_size / 100) as usize;
    // List of elements to build the tree structure
    let mut tree: FileTree = FileTree::with_capacity(estimated_records);

    // Create a CSV reader from the file
    let mut rdr = csv::Reader::from_reader(file_list_reader);

    fn add_recursively(
        tree: &mut FileTree,
        dir: &NcduDirectory,
        parent_index: usize,
    ) -> Result<(), Box<dyn Error>> {
        if let Some(NcduDirectoryEntry::InfoBlock(info)) = dir.get(0) {
            // Process the info block if needed
            let current_parent = tree.add_child(
                parent_index,
                &info.name,
                info.asize,
                get_date_modified_from_info(info),
                None,
                get_attributes(info, true, &info.name),
            );

            // Process the rest of the directory entries
            for entry in dir.iter().skip(1) {
                match entry {
                    NcduDirectoryEntry::InfoBlock(info) => {
                        // It's a file entry
                        tree.add_child(
                            current_parent,
                            &info.name,
                            info.asize,
                            get_date_modified_from_info(info),
                            None,
                            get_attributes(info, false, &info.name),
                        );
                    }
                    NcduDirectoryEntry::Directory(sub_dir) => {
                        // It's a sub-directory, recurse into it
                        add_recursively(tree, sub_dir, current_parent)?;
                    }
                }
            }
        } else {
            return Err("Invalid NCDU directory structure: missing InfoBlock".into());
        }

        Ok(())
    }

    // Iterate over the records and build the tree structure
    if let NcduDirectoryEntry::InfoBlock(info) = &data.3[0] {
        let root_index = tree.add_or_update_recursive(
            &info.name,
            info.asize,
            get_date_modified_from_info(info),
            None,
            get_attributes(info, true, &info.name),
        );
        for entry in data.3.iter().skip(1) {
            match entry {
                NcduDirectoryEntry::InfoBlock(info) => {
                    // It's a file entry
                    tree.add_child(
                        root_index,
                        &info.name,
                        info.asize,
                        get_date_modified_from_info(info),
                        None,
                        get_attributes(info, false, &info.name),
                    );
                }
                NcduDirectoryEntry::Directory(sub_dir) => {
                    // It's a sub-directory, recurse into it
                    add_recursively(&mut tree, sub_dir, root_index)?;
                }
            }
        }
    } else {
        return Err("Invalid NCDU top-level structure: missing InfoBlock".into());
    }

    // Reduce capacity to the actual number of elements
    tree.shrink_to_fit();
    // Return the elements as a vector
    Ok(tree)
}
